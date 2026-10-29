// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of multi-server vault ledger publishing, parent-digest CAS, grow-only revocations,
//! queued admission read-back, and steward lease coordination (`docs/DESIGN-alab-swarm-2026-10-27.md`
//! §3.1, §3.4, §8.2, C7).

use aterm_spec::derive::*;

/// `LedgerPublish`: two vault servers (`s1`, `s2`), one member (`m`), and two stewards (`st1`,
/// `st2`) operating across network partitions and heals.
///
/// Every non-genesis write names the digest(s) of the head(s) it read in `parents`, and each
/// server accepts a `PUT /v1/vault` only when its held head is in `parents` (CAS). When two
/// islands diverge during a partition and later heal, a steward merges both heads into a single
/// child (`seq = max + 1`, `parents = [H1, H2]`) whose revocation set is the grow-only union of
/// all parent branches. Queued admissions are retired only after reading back the published vault
/// and confirming the admission landed. Steward lease claims and takeovers publish through the
/// same parent-CAS path so at most one steward holds any lease epoch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ledger_publish_model() -> Model {
    aterm_spec::ty_model! {
        LedgerPublish {
            const Buggy = 0;

            var part = 0;
            var s1_seq = 1;
            var s2_seq = 1;
            var m_seq = 1;
            var s1_lineage_ok = 1;
            var s2_lineage_ok = 1;
            var ever_rev = 0;
            var s1_rev = 0;
            var s2_rev = 0;
            var q_adm = 0;
            var adm_live = 0;
            var adm_dropped = 0;
            var lease_ep = 0;
            var lease_exp = 0;
            var ep1_holders = 0;
            var ep2_holders = 0;
            var seq_fell = 0;

            action Partition when (part == 0 && s1_seq <= 2 && s2_seq <= 2) {
                part = 1;
            }
            action Heal when (part == 1) {
                part = 0;
            }
            action RevokeOnS1 when (ever_rev == 0 && s1_seq <= 2 &&
                (part == 1 || (s1_seq == s2_seq && s1_rev == s2_rev))) {
                s1_seq = s1_seq + 1;
                s2_seq = if part == 0 { s2_seq + 1 } else { s2_seq };
                s1_rev = 1;
                s2_rev = if part == 0 { 1 } else { s2_rev };
                ever_rev = 1;
            }
            action IslandAdvanceS2 when (part == 1 && s2_seq <= 2) {
                s2_seq = s2_seq + 1;
            }
            action QueueAdmission when (q_adm == 0 && adm_live == 0) {
                q_adm = 1;
            }
            action ApproveAndReadBack when (part == 0 && q_adm == 1 &&
                s1_seq == s2_seq && s1_rev == s2_rev && s1_seq <= 3) {
                s1_seq = s1_seq + 1;
                s2_seq = s2_seq + 1;
                q_adm = 0;
                adm_live = 1;
            }
            action MergeDivergedHeads when (part == 0 &&
                (s1_seq > s2_seq || s2_seq > s1_seq || s1_rev > s2_rev || s2_rev > s1_rev) &&
                s1_seq <= 3 && s2_seq <= 3) {
                s1_seq = if s2_seq <= s1_seq { s1_seq + 1 } else { s2_seq + 1 };
                s2_seq = if s2_seq <= s1_seq { s1_seq + 1 } else { s2_seq + 1 };
                s1_rev = if s1_rev + s2_rev > 0 { 1 } else { 0 };
                s2_rev = if s1_rev + s2_rev > 0 { 1 } else { 0 };
                adm_live = if q_adm == 1 { 1 } else { adm_live };
                q_adm = 0;
            }
            action MemberSync when (part == 0 && s1_seq == s2_seq && s1_seq > m_seq) {
                m_seq = s1_seq;
            }
            action ClaimLease when (part == 0 && s1_seq == s2_seq && s1_rev == s2_rev &&
                lease_ep == 0 && s1_seq <= 3) {
                lease_ep = 1;
                ep1_holders = 1;
                s1_seq = s1_seq + 1;
                s2_seq = s2_seq + 1;
            }
            action RenewLease when (part == 0 && s1_seq == s2_seq && s1_rev == s2_rev &&
                lease_ep == 1 && lease_exp == 0 && s1_seq <= 3) {
                s1_seq = s1_seq + 1;
                s2_seq = s2_seq + 1;
            }
            action ExpireLease when (lease_ep == 1 && lease_exp == 0) {
                lease_exp = 1;
            }
            action TakeoverLease when (part == 0 && s1_seq == s2_seq && s1_rev == s2_rev &&
                lease_ep == 1 && lease_exp == 1 && s1_seq <= 3) {
                lease_ep = 2;
                lease_exp = 0;
                ep2_holders = 1;
                s1_seq = s1_seq + 1;
                s2_seq = s2_seq + 1;
            }

            // Mutant dials (one independent protocol defect per §3.4 invariant):
            action AcceptLowerSeq when (Buggy == 1 && s1_seq > 1 && seq_fell == 0) {
                s1_seq = s1_seq - 1;
                seq_fell = 1;
            }
            action ClobberWithoutParentCas when (Buggy == 2 && part == 0 &&
                (s1_seq > s2_seq || s2_seq > s1_seq) && s1_rev == s2_rev && s1_lineage_ok == 1 &&
                s1_seq <= 3 && s2_seq <= 3) {
                s1_seq = if s2_seq <= s1_seq { s1_seq + 1 } else { s2_seq + 1 };
                s1_lineage_ok = 0;
            }
            action MergeDropsRevocation when (Buggy == 3 && part == 0 &&
                ever_rev == 1 && s1_rev == 1 && s2_rev == 0 && s1_seq <= s2_seq &&
                s1_seq <= 3 && s2_seq <= 3) {
                s1_seq = s2_seq + 1;
                s2_seq = s2_seq + 1;
                s1_rev = 0;
                s2_rev = 0;
            }
            action ApproveSkipsReadBack when (Buggy == 4 && part == 1 &&
                q_adm == 1 && adm_dropped == 0) {
                q_adm = 0;
                adm_dropped = 1;
            }
            action UncasedLeaseClaim when (Buggy == 5 && lease_ep == 1 && ep1_holders == 1) {
                ep1_holders = 2;
            }

            invariant SeqMonotonic: seq_fell == 0;
            invariant HeadDescendsFromPrior: s1_lineage_ok == 1 && s2_lineage_ok == 1;
            invariant RevocationsGrowOnly: ever_rev <= s1_rev + s2_rev;
            invariant QueuedAdmissionPublished: adm_dropped == 0;
            invariant SingleCurrentLeaseEpoch: ep1_holders <= 1 && ep2_holders <= 1;
            bound SeqAndEpochBox: s1_seq <= 5 && s2_seq <= 5 && m_seq <= 5 &&
                lease_ep <= 2 && ep1_holders <= 2 && ep2_holders <= 2;
        }
    }
}
