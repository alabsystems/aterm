// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One account its home Mac and a borrower both want (docs/DESIGN-fluid-swarm-2026-10-28.md F19;
/// the owner's ruling of 2026-11-11, "Accounts on their own machine"). H is one of the account's
/// homes, a Mac the lab registry says prefers it; B borrows it, and may word its row's claim
/// `home`, which no reader takes: a claim's kind is what the reader's lab names its Mac.
///
/// `hc`, `bc`: H's, B's row holds a live claim on the account. `hs`, `bs`: a session on H, B
/// runs on it. `bturn`: a turn end of B's session passed while H's claim was live, since B last
/// claimed. `lie`: B's row words its claim `home`. `old`: B's row ranks first-come (a build from
/// before homes ranked first, or a borrower that read no lab), and B ranks so. `hidle`: H gave its
/// claim up with its use and has not claimed since. `hwait`: H was refused the account for B's
/// lease. `bwait`: B was refused it while H had given its claim up.
///
/// A borrower claims only an account no home's claim holds, or keeps one it claimed first, so
/// wherever both claim it, B's claim is the earlier: first-come ranks B, home-first ranks H. H
/// ranks its own account home first whatever B's row says, so B's lease never refuses it. While
/// `old`, B keeps its earlier claim beside H, as both did before homes ranked first, so one user
/// past a turn holds of a B whose row ranks homes first; a change of B's build counts its turn ends
/// afresh.
///
/// Mutants: 1, first-come ranking, B's earlier lease refusing H; 2, a row's own word taken for
/// its kind, B's lie making it a home that first-come among homes ranks before H; 3, B keeping the
/// account at its turn end; 4, H's claim outliving its use; 5, B's first-come row making H rank
/// first-come, B's earlier lease refusing H while `old`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_claim_rank_model() -> Model {
    aterm_spec::ty_model! {
        AccountClaimRank {
            const Buggy = 0;
            var hc = 0; var bc = 0; var hs = 0; var bs = 0; var bturn = 0; var lie = 0;
            var old = 0; var hidle = 0; var hwait = 0; var bwait = 0;
            action HClaim when (hs == 0) {
                hwait = if bc == 1 && (Buggy == 1 || (Buggy == 2 && lie == 1)
                    || (Buggy == 5 && old == 1)) {1} else {hwait};
                hc = if bc == 1 && (Buggy == 1 || (Buggy == 2 && lie == 1)
                    || (Buggy == 5 && old == 1)) {hc} else {1};
                hs = if bc == 1 && (Buggy == 1 || (Buggy == 2 && lie == 1)
                    || (Buggy == 5 && old == 1)) {0} else {1};
                hidle = if bc == 1 && (Buggy == 1 || (Buggy == 2 && lie == 1)
                    || (Buggy == 5 && old == 1)) {hidle} else {0};
            }
            action HStop when (hs == 1) { hs = 0; }
            action HIdle when (hs == 0 && hc == 1) {
                hc = if Buggy == 4 {1} else {0};
                hidle = 1;
                bturn = if Buggy == 4 {bturn} else {0};
            }
            action BClaim when (bs == 0) {
                bwait = if hc == 1 && (old == 0 || bc == 0) && hidle == 1 {1} else {bwait};
                bc = if hc == 1 && (old == 0 || bc == 0) {bc} else {1};
                bs = if hc == 1 && (old == 0 || bc == 0) {0} else {1};
                bturn = if hc == 1 && (old == 0 || bc == 0) {bturn} else {0};
            }
            action BTurn when (bs == 1) {
                bturn = if hc == 1 {1} else {bturn};
                bs = if hc == 1 && old == 0 {if Buggy == 3 {bs} else {0}} else {bs};
            }
            action BIdle when (bs == 0 && bc == 1) { bc = 0; }
            action Lie { lie = if lie == 1 {0} else {1}; }
            action Old { old = if old == 1 {0} else {1}; bturn = 0; }
            invariant HomeNotRefusedByABorrower: hwait == 0;
            invariant OneUserPastATurn: hs == 0 || bs == 0 || bturn == 0 || old == 1;
            invariant NoIdleHomeHolds: bwait == 0;
        }
    }
}
