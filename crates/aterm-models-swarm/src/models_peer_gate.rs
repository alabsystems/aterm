// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One gated pull decision of a vault server's round: what a peer's TXT names of one vault
/// against what the server holds of it. `adv` is the TXT's `seq` below (0), equal to (1) or above
/// (2) the held one; `held` whether an envelope is held; `takes` whether the server takes the
/// vault at all (holding, pinning or holding records of it); `rec` the TXT's records digest:
/// none (0, an older build's TXT, which says nothing of its records), the held one (1), one a
/// finished pull at that peer proved (2), or another (3). The gate pulls a newer or first
/// envelope, records it lacks, a rec-less peer's records at every visit, and nothing else: TXT
/// suppresses only what it shows this server already holds or judged.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_gate_model() -> Model {
    aterm_spec::ty_model! {
        VaultPeerGate {
            const Buggy = 0;
            var adv = 1; var held = 1; var takes = 1; var rec = 1;
            var phase = 0; var env = 0; var recs = 0;
            action Lower when (phase == 0 && adv == 1) { adv = 0; }
            action Higher when (phase == 0 && adv == 1) { adv = 2; }
            action Unheld when (phase == 0 && held == 1) { held = 0; }
            action Stranger when (phase == 0 && takes == 1) { takes = 0; held = 0; }
            action NoRec when (phase == 0 && rec == 1) { rec = 0; }
            action ProvenRec when (phase == 0 && rec == 1) { rec = 2; }
            action OtherRec when (phase == 0 && rec == 1) { rec = 3; }
            action Gate when (phase == 0) {
                phase = 1;
                env = if Buggy == 4 {1}
                    else if takes == 0 && Buggy == 5 {1}
                    else if takes == 0 {0}
                    else if adv == 2 {1}
                    else if held == 0 && Buggy == 1 {0}
                    else if held == 0 {1}
                    else {0};
                recs = if Buggy == 4 {1}
                    else if takes == 0 && Buggy == 5 {1}
                    else if takes == 0 {0}
                    else if rec == 0 && Buggy == 3 {0}
                    else if rec == 0 {1}
                    else if rec == 3 && Buggy == 2 {0}
                    else if rec == 3 {1}
                    else if rec == 2 && Buggy == 6 {1}
                    else {0};
            }
            invariant EnvelopeProgress: phase == 0 || takes == 0 || (held == 1 && 2 > adv)
                || env == 1;
            invariant RecordsProgress: phase == 0 || takes == 0 || 3 > rec || recs == 1;
            invariant RecLessProgress: phase == 0 || takes == 0 || rec > 0 || recs == 1;
            invariant QuietWhenHeld: phase == 0 || held == 0 || adv == 2 || rec == 3 || rec == 0
                || (env == 0 && recs == 0);
            invariant NothingUntaken: phase == 0 || takes == 1 || (env == 0 && recs == 0);
            bound GateBox: adv <= 2 && held <= 1 && takes <= 1 && rec <= 3 && phase <= 1
                && env <= 1 && recs <= 1;
        }
    }
}
