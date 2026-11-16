// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One gated pull decision of a vault server's round: what a peer's TXT names of one vault
/// against what the server holds of it. `adv` is the TXT's `seq` below (0), equal to (1) or above
/// (2) the held one; `held` whether an envelope is held; `takes` whether the server takes the
/// vault at all (holding, pinning or holding records of it); `rec` the TXT's records digest:
/// none (0, an older build's TXT, which says nothing of its records), the held one (1), one a
/// finished pull at that peer proved (2), or another (3); `inst` the instance the TXT is on: one
/// under any name but `alab-secrets` (0), an `alab-secrets` instance where the instance its
/// `label` names (its server's own-label instance) did not name a `rec` for the vault at its
/// endpoints, in the round or one within a sweep period before it with no catch-up since (1:
/// its server's only registration, whose TXT names no label; one whose own-label instance no
/// round in that period resolved naming the records, as one naming another vault, which a
/// round's resolve leaves out; or one at endpoints where only another server was noted), or one
/// where it did (2: that instance's TXT makes the records due by its own `rec` in each round
/// that resolves it). `inst` is the decision's input as the server's gate takes it
/// (`named_elsewhere`); how a round finds that instance, among those it resolved and those it
/// remembers, is not modelled. The gate pulls a newer or first envelope, records it lacks, a
/// rec-less peer's records at every visit unless that peer is an `alab-secrets` instance where
/// that instance named a `rec` for the vault, and nothing else: TXT suppresses only what it
/// shows this server already holds or judged, and the records the instance an `alab-secrets`
/// TXT's label names, at the same endpoints, names.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_gate_model() -> Model {
    aterm_spec::ty_model! {
        VaultPeerGate {
            const Buggy = 0;
            var adv = 1; var held = 1; var takes = 1; var rec = 1; var inst = 0;
            var phase = 0; var env = 0; var recs = 0;
            action Lower when (phase == 0 && adv == 1) { adv = 0; }
            action Higher when (phase == 0 && adv == 1) { adv = 2; }
            action Unheld when (phase == 0 && held == 1) { held = 0; }
            action Stranger when (phase == 0 && takes == 1) { takes = 0; held = 0; }
            action NoRec when (phase == 0 && rec == 1) { rec = 0; }
            action ProvenRec when (phase == 0 && rec == 1) { rec = 2; }
            action OtherRec when (phase == 0 && rec == 1) { rec = 3; }
            action AlabAlone when (phase == 0 && inst == 0) { inst = 1; }
            action AlabNamedElsewhere when (phase == 0 && inst == 0) { inst = 2; }
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
                    else if rec == 0 && inst == 2 && Buggy == 8 {1}
                    else if rec == 0 && inst == 2 {0}
                    else if rec == 0 && inst == 1 && Buggy == 7 {0}
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
            invariant RecLessProgress: phase == 0 || takes == 0 || rec > 0 || inst == 2
                || recs == 1;
            invariant QuietWhenHeld: phase == 0 || held == 0 || adv == 2 || rec == 3
                || (rec == 0 && 2 > inst) || (env == 0 && recs == 0);
            invariant NothingUntaken: phase == 0 || takes == 1 || (env == 0 && recs == 0);
            bound GateBox: adv <= 2 && held <= 1 && takes <= 1 && rec <= 3 && inst <= 2
                && phase <= 1 && env <= 1 && recs <= 1;
        }
    }
}
