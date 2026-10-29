// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two serialized local observers of the same account, an empty third observer,
/// durable reloads and time advancing through both evidence deadlines. This assumes
/// the private lock and atomic file primitive, not a distributed clock or provider fence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_claim_activity_model() -> Model {
    aterm_spec::ty_model! {
        AccountClaimActivity {
            const Buggy = 0;
            var now = 0; var a = 0; var b = 0; var empty = 0; var until = 0; var live = 0; var delayed = 0;
            action ObserveA when (now == 0 && a == 0) {
                a = 1;
                until = if Buggy == 6 {until} else {if Buggy == 1 || 1 > until {1} else {until}};
                live = if Buggy == 6 {live} else {1};
            }
            action ObserveB when (now == 0 && b == 0) {
                b = 1;
                until = if Buggy == 5 {until} else {2};
                live = if Buggy == 5 {live} else {1};
            }
            action Empty when (empty == 0) {
                empty = 1;
                until = if Buggy == 2 {0} else {until};
                live = if Buggy == 2 {0} else {live};
            }
            action Tick when (2 > now) {
                now = now + 1;
                live = if Buggy == 3 {live} else {
                    if until > now + 1 || (Buggy == 4 && until == now + 1 && until > 0) {1} else {0}
                };
                until = if Buggy == 3 || until > now + 1 {until} else {0};
            }
            action Delayed when (now > 0 && delayed == 0) {
                delayed = 1;
                now = if Buggy == 7 {0} else {now};
            }
            invariant NoEarlyRelease: ((a == 0 || 1 <= now) && (b == 0 || 2 <= now)) || live == 1;
            invariant IdleRelease: (a == 1 && 1 > now) || (b == 1 && 2 > now) || live == 0;
            invariant DeadlineUnion: until == (if b == 1 && 2 > now {2} else {if a == 1 && 1 > now {1} else {0}});
            invariant ClockFloor: delayed == 0 || now > 0;
        }
    }
}

/// A readable peer snapshot stays authoritative when local publication fails.
/// Faults cover the local activity gate/save and known unwritable/full claim tables;
/// skew is absent, a delayed call, or beyond the accepted local clock bound.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_claim_observation_model() -> Model {
    aterm_spec::ty_model! {
        AccountClaimObservation {
            const Buggy = 0;
            var phase = 0; var fault = 0; var peer = 0; var skew = 0;
            var observed = 0; var accepted = 0; var intent = 0; var own_expired = 0;
            action Fault when (phase == 0 && own_expired == 0 && 4 > fault) { fault = fault + 1; }
            action Peer when (phase == 0 && own_expired == 0 && peer == 0) { peer = 1; }
            action Skew when (phase == 0 && own_expired == 0 && 2 > skew) { skew = skew + 1; }
            action OwnExpired when (phase == 0 && fault == 0 && peer == 1 && skew == 1 && own_expired == 0) { own_expired = 1; }
            action Touch when (phase == 0) {
                phase = 1;
                observed = if (Buggy == 1 && fault > 0) || (Buggy == 2 && skew > 0) {0} else {peer};
                accepted = if Buggy == 7 && own_expired == 1 {1} else {if Buggy == 6 {0} else {if peer == 0 && (
                    (fault == 0 && 2 > skew)
                    || (Buggy == 3 && fault == 1 && skew == 0)
                    || (Buggy == 4 && fault == 0 && skew == 2)
                ) {1} else {0}}};
                intent = if Buggy == 7 && own_expired == 1 {1} else {if Buggy == 6 {0} else {if Buggy == 5 || (fault == 0 && peer == 0 && 2 > skew) {1} else {0}}};
            }
            invariant PeerObserved: phase == 0 || peer == 0 || observed == 1;
            invariant PeerBlocksNewGrant: phase == 0 || peer == 0 || accepted == 0;
            invariant LocalFailureCannotPublish: phase == 0 || fault == 0 || accepted == 0;
            invariant LargeSkewCannotPublish: phase == 0 || 2 > skew || accepted == 0;
            invariant RejectedCandidateNoNewIntent: phase == 0 || accepted == 1 || intent == 0;
            invariant ReadyProgress: phase == 0 || fault > 0 || peer == 1 || skew == 2 || accepted == 1;
        }
    }
}
