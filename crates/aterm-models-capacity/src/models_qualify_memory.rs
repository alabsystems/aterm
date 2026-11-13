// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Qualify's memory admission, in hundredths of a GiB, sized as on m3.

use aterm_spec::derive::*;

/// THE QUALIFY'S SHARED SLICE BUDGET AND ADMISSION BARRIER. Before compiler work starts, one
/// parent reserves the retained compiler budget plus 2 GiB for each slice driver. The committed
/// configuration is the accepted 43/47 GiB measurement; the declared 52/56 GiB scenario applies
/// current production's 20% margin to the larger historical unit peaks. The authenticated Tippy
/// frontend does not permit an outer compiler wrapper, so the
/// verifier retains and claims lint's whole measured peak, never an unenforced driver-only
/// envelope. While every non-export jobserver token is held, a whole-lint claim which fits beside
/// the parent gives build every token but one and tree one. If that early claim queues, the
/// barrier opens only for the bounded serial escape:
/// the slices finish and release their parent before lint enters. Slice compilers allocate only
/// from the private budget, so the retained native and compat links serialize while their smaller
/// units can overlap. Lint releases its whole claim before the test compile's
/// driver and units allocate from the host memory left around the parent. Once both slices finish, the parent
/// gives its whole reservation back; its private allocations never become duplicate global
/// claims.
///
/// `SliceUnitsStayWithinBudget` keeps both links from overlapping.
/// `CompilerWorkFollowsMemoryDecision` binds work to a held or explicitly queued lint request.
/// `ParentCoversBothDrivers` keeps the parent from reserving only the compiler budget.
/// `FitsRealMemory` bounds every retained claim by the measured 75.8 GiB.
/// `BuildKeepsCriticalWidth` and `OneTreeTokenBesideBuild` bind that decision to the 13/1
/// compiler split where Tippy fits. In the margined scenario 56 + 28 GiB exceeds m3's 75.8 GiB,
/// so the claim queues and all 14 non-export tokens stay with build until the parent leaves.
///
/// `Buggy = 1` overlaps both links. `Buggy = 2` starts compiler work after the parent but before
/// lint is reserved (the old delayed-admission order). `Buggy = 3` admits a parent that omits
/// both drivers. `Buggy = 4` admits two small tree units beside a held heavy unit despite the
/// one-token share and residual host bound.
/// `Buggy = 5` transfers a critical-path token from build to tree.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn qualify_memory_model() -> Model {
    aterm_spec::ty_model! {
        QualifyMemory {
            const Buggy = 0;
            const Memory = 7580;
            const SliceBudget = 4300;
            const Driver = 200;
            const Parent = 4700;
            const NativeLink = 4300;
            const CompatLink = 2400;
            const SliceUnit = 200;
            // Current production's margin applied to the historical 43.046/24.113-GiB inputs.
            // This is the bounded serial corner on m3; the committed configuration retains the
            // measured concurrent corner so both live scheduling paths remain non-vacuous.
            scenario SliceBudget = 5200, Parent = 5600,
                NativeLink = 5200, CompatLink = 2900;
            // A whole-lint claim as m3 measures it (a retained peak near 23.3 GiB and a fifth
            // more), while the parent and lint together remain inside m3's 75.8-GiB release
            // bound. The claim is the verifier's measurement on each machine, not a constant;
            // one never measured queues to run alone, the `WholeLintQueues` path.
            const WholeLint = 2800;
            const TreeDriver = 200;
            const TreeUnit = 200;
            const TreeHeavy = 2352;
            const NonExport = 14;
            const BuildFirst = 13;

            // Parent: 0 not admitted, 1 held, 2 released after both slices ended.
            var parent = 0;
            var parent_reserved = 0;
            // Lint: 0 not requested, 2 released, 3 whole-stage claim queued, 4 held.
            var lint = 0;
            // The non-export jobserver tokens: 0 held, 1 released after the parent is held and
            // either a lint reservation or the bounded serial-escape decision is visible.
            var gate = 0;
            // FIFO allocations remain behind the gate. A held early tree claim releases 13/1;
            // a queued one releases 14/0 and transfers that grant after the slices end.
            var build_tokens = 0;
            var tree_tokens = 0;
            var serial = 0;
            var build_ended = 0;
            // What the capacity ledger globally holds: the parent plus tree claims.
            var global_held = 0;
            // Each slice: 0 before its small unit, 1 small running, 2 link ready,
            // 3 link running, 4 complete.
            var native = 0;
            var compat = 0;
            // Compiler memory allocated inside the parent's private SliceBudget.
            var slice_held = 0;
            // Tree: 0 not started, 1 lint/preflight, 2 test compile, 3 compile complete,
            // 4 complete. The test compile has three small units and one heavy unit; the lawful
            // schedule can run the heavy unit beside one small one.
            var tree = 0;
            var tree_left = 3;
            var tree_small = 0;
            var tree_heavy = 0;
            // Temporal defects need explicit history; a later lawful state must not hide them.
            var early_work = 0;

            action ParentAdmit when (parent == 0 && global_held + Parent <= Memory) {
                parent = 1;
                parent_reserved = Parent;
                global_held = global_held + Parent;
            }
            action WholeLintAdmit when (parent == 1 && lint == 0
                && global_held + WholeLint <= Memory) {
                lint = 4;
                gate = 1;
                build_tokens = BuildFirst;
                tree_tokens = NonExport - BuildFirst;
                global_held = global_held + WholeLint;
            }
            // External holders are outside this model's accounting. This nondeterministic arm is
            // the production observation of the exact queued claim: slice work may finish, but
            // tree work remains shut until its parent has left and lint is actually held.
            action WholeLintQueues when (parent == 1 && lint == 0) {
                lint = 3;
                gate = 1;
                build_tokens = NonExport;
                tree_tokens = 0;
                serial = 1;
            }
            action QueuedWholeLintAdmits when (parent > 0 && lint == 3
                && global_held + WholeLint <= Memory) {
                lint = 4;
                global_held = global_held + WholeLint;
            }
            action NativeSmallStarts when (gate == 1 && build_tokens > 0
                && parent == 1 && native == 0
                && slice_held + SliceUnit <= SliceBudget) {
                native = 1;
                slice_held = slice_held + SliceUnit;
            }
            action CompatSmallStarts when (gate == 1 && build_tokens > 0
                && parent == 1 && compat == 0
                && slice_held + SliceUnit <= SliceBudget) {
                compat = 1;
                slice_held = slice_held + SliceUnit;
            }
            action NativeSmallEnds when (native == 1) {
                native = 2;
                slice_held = slice_held - SliceUnit;
            }
            action CompatSmallEnds when (compat == 1) {
                compat = 2;
                slice_held = slice_held - SliceUnit;
            }
            action NativeLinkStarts when (native == 2
                && slice_held + NativeLink <= SliceBudget) {
                native = 3;
                slice_held = slice_held + NativeLink;
            }
            action CompatLinkStarts when (compat == 2
                && slice_held + CompatLink <= SliceBudget) {
                compat = 3;
                slice_held = slice_held + CompatLink;
            }
            action NativeLinkEnds when (native == 3) {
                native = 4;
                slice_held = slice_held - NativeLink;
            }
            action CompatLinkEnds when (compat == 3) {
                compat = 4;
                slice_held = slice_held - CompatLink;
            }
            action ParentEnds when (parent == 1 && native == 4 && compat == 4) {
                parent = 2;
                parent_reserved = 0;
                global_held = global_held - Parent;
            }
            action BuildEnds when (parent == 2 && build_ended == 0) {
                build_ended = 1;
                tree_tokens = tree_tokens + build_tokens;
                build_tokens = 0;
            }

            // `parent == 2` means the reservation was held before the tree but both slices
            // completed before the tree thread observed it. Starting the tree adds no claim:
            // the whole lint is already covered.
            action TreeStarts when (tree == 0 && gate == 1
                && tree_tokens > 0 && lint == 4 && parent > 0) {
                tree = 1;
            }
            action WholeLintEnds when (tree == 1 && lint == 4) {
                lint = 2;
                global_held = global_held - WholeLint;
            }
            action TreeCompileStarts when (tree == 1 && lint == 2
                && global_held + TreeDriver <= Memory) {
                tree = 2;
                global_held = global_held + TreeDriver;
            }
            action TreeSmallStarts when (tree == 2 && tree_left > 0
                && (((tree_heavy == 0 || tree_heavy == 2) && tree_tokens > tree_small)
                    || (tree_heavy == 1 && tree_tokens > tree_small + 1))
                && global_held + TreeUnit <= Memory) {
                tree_left = tree_left - 1;
                tree_small = tree_small + 1;
                global_held = global_held + TreeUnit;
            }
            action TreeSmallEnds when (tree_small > 0) {
                tree_small = tree_small - 1;
                global_held = global_held - TreeUnit;
            }
            action TreeHeavyStarts when (tree == 2 && tree_heavy == 0
                && tree_tokens > tree_small
                && global_held + TreeHeavy <= Memory) {
                tree_heavy = 1;
                global_held = global_held + TreeHeavy;
            }
            action TreeHeavyEnds when (tree_heavy == 1) {
                tree_heavy = 2;
                global_held = global_held - TreeHeavy;
            }
            action TreeCompileEnds when (tree == 2 && tree_left == 0 && tree_small == 0
                && tree_heavy == 2) {
                tree = 3;
                global_held = global_held - TreeDriver;
            }
            action TreeEnds when (tree == 3) { tree = 4; }

            action BothLinksOverlap when (Buggy == 1 && native == 3 && compat == 2) {
                compat = 3;
                slice_held = slice_held + CompatLink;
            }
            action CompilerStartsBeforeLint when (Buggy == 2 && parent == 1 && lint == 0
                && native == 0 && slice_held + SliceUnit <= SliceBudget) {
                native = 1;
                early_work = 1;
                slice_held = slice_held + SliceUnit;
            }
            action ParentOmitsDrivers when (Buggy == 3 && parent == 0
                && global_held + SliceBudget <= Memory) {
                parent = 1;
                parent_reserved = SliceBudget;
                global_held = global_held + SliceBudget;
            }
            action TreeTakesReservedMemory when (Buggy == 4 && tree == 2
                && parent == 1 && tree_heavy == 1 && tree_left > 1) {
                tree_left = tree_left - 2;
                tree_small = tree_small + 2;
                global_held = global_held + TreeUnit + TreeUnit;
            }
            action TreeStealsBuildWidth when (Buggy == 5 && parent == 1 && gate == 1
                && build_tokens == BuildFirst && tree_tokens == NonExport - BuildFirst) {
                build_tokens = build_tokens - 1;
                tree_tokens = tree_tokens + 1;
            }

            invariant SliceUnitsStayWithinBudget: slice_held <= SliceBudget;
            invariant CompilerWorkFollowsMemoryDecision: early_work == 0;
            invariant ParentCoversBothDrivers: parent == 0 || parent > 1
                || SliceBudget + Driver + Driver <= parent_reserved;
            invariant FitsRealMemory: global_held <= Memory;
            invariant TokenAllocationIsExact:
                (gate == 0 && build_tokens == 0 && tree_tokens == 0)
                || (gate == 1 && build_ended == 0 && serial == 0
                    && build_tokens == BuildFirst && tree_tokens == NonExport - BuildFirst)
                || (gate == 1 && build_ended == 0 && serial == 1
                    && build_tokens == NonExport && tree_tokens == 0)
                || (gate == 1 && build_ended == 1
                    && build_tokens == 0 && tree_tokens == NonExport);
            invariant BuildKeepsCriticalWidth: gate == 0 || build_ended == 1
                || BuildFirst <= build_tokens;
            invariant OneTreeTokenBesideBuild: build_ended == 1 || tree_tokens <= 1;
            bound StateIsFinite: parent <= 2 && lint <= 4 && gate <= 1
                && native <= 4 && compat <= 4 && tree <= 4
                && tree_left <= 3 && tree_small <= 3 && tree_heavy <= 2 && early_work <= 1
                && build_tokens <= NonExport && tree_tokens <= NonExport
                && serial <= 1 && build_ended <= 1
                && parent_reserved <= Parent && slice_held <= NativeLink + CompatLink
                && global_held <= Memory + TreeUnit;
        }
    }
}
