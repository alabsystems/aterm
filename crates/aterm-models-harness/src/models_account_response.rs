// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A response can update only its captured move, after its durable observation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_response_model() -> Model {
    aterm_spec::ty_model! {
        AccountResponse {
            const Buggy = 0;
            var phase = 0; var reply = 0; var captured = 1; var entry = 1;
            var generation = 1; var receipt = 1; var process = 1; var origin = 1;
            var pending = 0; var present = 1; var notice = 0; var enriched = 0;
            var durable = 0; var published = 0; var remaining = 1; var answered = 0;
            action Absent when (phase == 0 && captured == 1) {
                captured = 0; present = 0; remaining = 0;
            }
            action StaleEntry when (phase == 0 && entry == 1) { entry = 2; }
            action Serve when (phase == 0) { phase = 1; reply = 1; }
            action Fail when (phase == 0) { phase = 1; reply = 2; }
            action NewGeneration when (phase > 0 && phase <= 2 && generation == 1) {
                generation = 2;
            }
            action NewReceipt when (phase > 0 && phase <= 2 && receipt == 1) {
                receipt = 2; present = 1; remaining = 1;
            }
            action NewProcess when (phase > 0 && phase <= 2 && process == 1) { process = 2; }
            action OtherOrigin when (phase > 0 && phase <= 2 && origin == 1) { origin = 2; }
            action StartMove when (phase > 0 && phase <= 2 && pending == 0) { pending = 1; }
            action Notice when (phase > 0 && phase <= 2 && notice == 0) { notice = 1; }
            action Enrich when (phase > 0 && phase <= 2 && enriched == 0) { enriched = 1; }
            action Commit when (phase == 1) { phase = 2; durable = 1; }
            action Refuse when (phase == 1) { phase = 2; durable = 0; }
            action Publish when (phase == 2) {
                phase = 3;
                published = if Buggy == 1 && generation == 2 {1}
                    else {if Buggy == 2 && receipt == 2 {1}
                    else {if Buggy == 3 && process == 2 {1}
                    else {if Buggy == 4 && origin == 2 {1}
                    else {if Buggy == 5 && pending == 1 {1}
                    else {if Buggy == 6 && captured == 0 {1}
                    else {if Buggy == 7 && durable == 0 {1}
                    else {if Buggy == 8 && entry == 2 {1}
                    else {if Buggy == 12 {0}
                    else {if captured == 1 && entry == 1 && generation == 1
                        && receipt == 1 && process == 1 && origin == 1
                        && pending == 0 && present == 1 && durable == 1 {1} else {0}}}}}}}}}};
                remaining = if Buggy == 9 && reply == 1 {1}
                    else {if Buggy == 11 && durable == 0 {0}
                    else {if reply == 1 && captured == 1 && entry == 1 && generation == 1
                        && receipt == 1 && process == 1 && origin == 1
                        && pending == 0 && present == 1 && durable == 1 {0} else {present}}};
                answered = if Buggy == 10 {0}
                    else {if reply == 2 && captured == 1 && entry == 1 && generation == 1
                        && receipt == 1 && process == 1 && origin == 1
                        && pending == 0 && present == 1 && durable == 1 {1} else {0}};
            }
            invariant SameGeneration: generation == 1 || published == 0;
            invariant SameReceipt: receipt == 1 || published == 0;
            invariant SameProcess: process == 1 || published == 0;
            invariant SameOrigin: origin == 1 || published == 0;
            invariant NoPendingMove: pending == 0 || published == 0;
            invariant CapturedMove: captured == 1 || published == 0;
            invariant DurableFirst: durable == 1 || published == 0;
            invariant EntryMatches: entry == 1 || published == 0;
            invariant ServedConsumes: phase <= 2 || reply == 2 || published == 0 || remaining == 0;
            invariant FailedAnswers: phase <= 2 || reply == 1 || published == 0 || answered == 1;
            invariant NoCollateral: phase <= 2 || published == 1 || (remaining == present && answered == 0);
            invariant CurrentCompletes: phase <= 2 || captured == 0 || entry == 2
                || generation == 2 || receipt == 2 || process == 2 || origin == 2
                || pending == 1 || present == 0 || durable == 0 || published == 1;
        }
    }
}
