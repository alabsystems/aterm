// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The lab registry's typed cells (docs/DESIGN-alab-lab-model-2026-11-01.md §0, §3, §11 step 12),
/// one law to a walk: the first step chooses the area (`area`), and only that area's actions run.
///
/// 1. **One register, two writers** (`aterm_db::documents::apply`, `typed::cell::project`). Each
///    writer's latest write is at most one candidate (`ca`, `cb`), holding `1` (true, an add) or
///    `2` (false, a removal) (`va`, `vb`); `ka` says writer A has observed B's latest write, and
///    `kb` the reverse. A write replaces its own earlier candidate and, where it observed it, the
///    other writer's; one it did not observe stays (`lost` records a dropped one, `stale` a kept
///    one the write observed). The register is read twice, as a scalar column (`shown`: how many
///    distinct values, equal ones collapsing) and as a set element (`present`: add-wins, so any
///    adding candidate keeps the element).
/// 2. **Owned rows** (`typed::owned::read_owned`). Mac A writes its own row (`oa` rows, the
///    latest `pa` projected), Mac B its own (`ob`, `pb`), and A once writes a row at B's key
///    (`ForgeB`); `forged` records that row projecting.
/// 3. **Columns across builds** (`typed::cell::change`, `project`). A version-2 build writes the
///    version-2 column `region`; an old build edits another column of the row. `kept` is the
///    column as the new build reads it, `raw` as the old build reads it (shown raw, read-only).
/// 4. **The per-Mac quota** (`typed::owned::quota`, `self_quota` 2). `spent` is the records A's
///    automatic writes cost, each one plus the causal-only merges (`joins`) appended just before
///    it; `live` the cost of those that project; owner records and reverts (`owner`) never count,
///    and `oover` records one past the quota.
/// 5. **A preference never limits** (§0; `aterm_agent::harness::alab_roster::{fleet_for, retain}`
///    over `accounts_sync::build_machine_roster`). One account on this Mac: `pref` this Mac
///    prefers it, `joinable` it may join unasked, `row` it has a row here, `held` an old pool
///    file's hold on that row. A sync keeps every row, adds the account where it is preferred and
///    joinable, adds nothing unasked and clears every hold: `dropped`, `unasked`, `missing` and
///    `limited` record the opposite.
///
/// Mutants: 1 a write drops the other writer's candidate unobserved (a whole-register last
/// write), 2 a write that observed the other's candidate keeps it, 3 equal values never
/// collapse, 4 a removal wins over a concurrent add, 5 a row at another Mac's key projects,
/// 6 each key keeps its first row, 7 an old build's edit rewrites the whole row from its schema,
/// 8 an old build's reader drops a column it does not know, 9 merges cost nothing, 10 owner
/// records count against the quota, 11 reverts do, 12 a sync drops a row this Mac does not
/// prefer, 13 a sync adds every live account, 14 a sync keeps an old hold, 15 a sync leaves out
/// an account this Mac prefers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alab_cells_model() -> Model {
    aterm_spec::ty_model! {
        AlabCells {
            const Buggy = 0;
            var area = 0;
            var writes = 0;
            var ca = 0; var cb = 0; var va = 0; var vb = 0; var ka = 1; var kb = 1;
            var shown = 0; var present = 0; var lost = 0; var stale = 0;
            var oa = 0; var ob = 0; var pa = 0; var pb = 0; var forged = 0; var forgery = 0;
            var region = 0; var kept = 0; var raw = 0;
            var spent = 0; var joins = 0; var live = 0; var owner = 0; var oover = 0;
            var pref = 0; var joinable = 1; var row = 0; var held = 0;
            var dropped = 0; var unasked = 0; var missing = 0; var limited = 0;

            action ChooseRegister when (area == 0) { area = 1; }
            action ChooseOwned when (area == 0) { area = 2; }
            action ChooseColumns when (area == 0) { area = 3; }
            action ChooseQuota when (area == 0) { area = 4; }
            action ChoosePreference when (area == 0) { area = 5; }

            // 1. The register. A writes true (`WriteA1`) or false (`WriteA2`); B likewise.
            action WriteA1 when (area == 1 && 3 > writes) {
                writes = writes + 1; ca = 1; va = 1; kb = 0;
                cb = if (ka == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {cb};
                lost = if Buggy == 1 && ka == 0 && cb == 1 {1} else {lost};
                stale = if Buggy == 2 && ka == 1 && cb == 1 {1} else {stale};
                shown = if (if (ka == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {cb}) == 1
                    && ((1 > vb || vb > 1) || Buggy == 3) {2} else {1};
                present = if Buggy == 4 && (if (ka == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {cb}) == 1
                    && vb == 2 {0} else {1};
            }
            action WriteA2 when (area == 1 && 3 > writes) {
                writes = writes + 1; ca = 1; va = 2; kb = 0;
                cb = if (ka == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {cb};
                lost = if Buggy == 1 && ka == 0 && cb == 1 {1} else {lost};
                stale = if Buggy == 2 && ka == 1 && cb == 1 {1} else {stale};
                shown = if (if (ka == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {cb}) == 1
                    && ((2 > vb || vb > 2) || Buggy == 3) {2} else {1};
                present = if (4 > Buggy || Buggy > 4) && (if (ka == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {cb}) == 1
                    && vb == 1 {1} else {0};
            }
            action WriteB1 when (area == 1 && 3 > writes) {
                writes = writes + 1; cb = 1; vb = 1; ka = 0;
                ca = if (kb == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {ca};
                lost = if Buggy == 1 && kb == 0 && ca == 1 {1} else {lost};
                stale = if Buggy == 2 && kb == 1 && ca == 1 {1} else {stale};
                shown = if (if (kb == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {ca}) == 1
                    && ((1 > va || va > 1) || Buggy == 3) {2} else {1};
                present = if Buggy == 4 && (if (kb == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {ca}) == 1
                    && va == 2 {0} else {1};
            }
            action WriteB2 when (area == 1 && 3 > writes) {
                writes = writes + 1; cb = 1; vb = 2; ka = 0;
                ca = if (kb == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {ca};
                lost = if Buggy == 1 && kb == 0 && ca == 1 {1} else {lost};
                stale = if Buggy == 2 && kb == 1 && ca == 1 {1} else {stale};
                shown = if (if (kb == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {ca}) == 1
                    && ((2 > va || va > 2) || Buggy == 3) {2} else {1};
                present = if (4 > Buggy || Buggy > 4) && (if (kb == 1 && (2 > Buggy || Buggy > 2)) || Buggy == 1 {0} else {ca}) == 1
                    && va == 1 {1} else {0};
            }
            // A writer receives the other's latest write, so its next write observes it.
            action SeeA when (area == 1 && ka == 0) { ka = 1; }
            action SeeB when (area == 1 && kb == 0) { kb = 1; }

            // 2. Owned rows: each Mac's own key, and one row A writes at B's key (n = 3).
            action OwnA when (area == 2 && 2 > oa) {
                oa = oa + 1; pa = if Buggy == 6 && oa > 0 {pa} else {oa + 1};
            }
            action OwnB when (area == 2 && 2 > ob) {
                ob = ob + 1;
                pb = if (Buggy == 6 && ob > 0) || (Buggy == 5 && forged == 1) {pb} else {ob + 1};
            }
            action ForgeB when (area == 2 && forgery == 0) {
                forgery = 1;
                forged = if Buggy == 5 {1} else {forged};
                pb = if Buggy == 5 {3} else {pb};
            }

            // 3. A version-2 column beside an old build's edits.
            action NewRegion when (area == 3 && region == 0) {
                region = 1; kept = 1; raw = if Buggy == 8 {0} else {1};
            }
            action OldEdit when (area == 3 && 3 > writes) {
                writes = writes + 1;
                kept = if Buggy == 7 {0} else {kept};
                raw = if Buggy == 7 {0} else {raw};
            }
            action NewEdit when (area == 3 && 3 > writes) { writes = writes + 1; }

            // 4. The quota of two automatic records.
            action Merge when (area == 4 && joins == 0 && 3 > spent) { joins = 1; }
            action Automatic when (area == 4 && 3 > spent) {
                spent = spent + (if Buggy == 9 {1} else {1 + joins});
                live = live + (if spent + (if Buggy == 9 {1} else {1 + joins}) <= 2 {1 + joins} else {0});
                joins = 0;
            }
            action Owner when (area == 4 && 2 > owner) {
                owner = owner + 1; joins = 0;
                spent = if Buggy == 10 {spent + 1} else {spent};
                oover = if Buggy == 10 && spent + 1 > 2 {oover + 1} else {oover};
            }
            action Revert when (area == 4 && 2 > owner) {
                owner = owner + 1; joins = 0;
                spent = if Buggy == 11 {spent + 1} else {spent};
                oover = if Buggy == 11 && spent + 1 > 2 {oover + 1} else {oover};
            }

            // 5. Preference: the owner's verbs and a person's acts, then a sync.
            action Prefer when (area == 5 && pref == 0) { pref = 1; }
            action Unprefer when (area == 5 && pref == 1) { pref = 0; }
            action Pause when (area == 5 && joinable == 1) { joinable = 0; }
            action Ask when (area == 5 && row == 0) { row = 1; }
            action Hold when (area == 5 && row == 1 && held == 0) { held = 1; }
            action Sync when (area == 5) {
                row = if Buggy == 12 {if pref == 1 && joinable == 1 {1} else {0}}
                    else {if Buggy == 13 {if row == 1 || joinable == 1 {1} else {0}}
                    else {if Buggy == 15 {row}
                    else {if row == 1 || (pref == 1 && joinable == 1) {1} else {0}}}};
                dropped = if Buggy == 12 && row == 1 && (pref == 0 || joinable == 0) {1} else {dropped};
                unasked = if Buggy == 13 && row == 0 && pref == 0 && joinable == 1 {1} else {unasked};
                missing = if Buggy == 15 && row == 0 && pref == 1 && joinable == 1 {1} else {missing};
                held = if Buggy == 14 {held} else {0};
                limited = if Buggy == 14 && held == 1 {1} else {limited};
            }

            invariant ObservedOnly: lost == 0;
            invariant ObservedResolves: stale == 0;
            invariant EqualValuesCollapse: shown == (if ca == 1 && cb == 1 && (vb > va || va > vb) {2}
                else {if ca == 1 || cb == 1 {1} else {0}});
            invariant AddWins: present == (if (ca == 1 && va == 1) || (cb == 1 && vb == 1) {1} else {0});
            invariant ForeignRowInert: forged == 0;
            invariant OwnRowLatest: pa == oa && (forged == 1 || pb == ob);
            invariant NewerColumnKept: kept == region;
            invariant NewerColumnShownRaw: raw == region;
            invariant AutomaticWithinQuota: live <= 2;
            invariant OwnerNeverInert: oover == 0;
            invariant RowNeverDropped: dropped == 0;
            invariant NothingUnasked: unasked == 0;
            invariant PreferredJoins: missing == 0;
            invariant NoHoldUnderLab: limited == 0;
        }
    }
}
