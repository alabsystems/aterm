// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Manual account metadata and its local fleet share one recoverable intent.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_manual_receipt_model() -> Model {
    aterm_spec::ty_model! {
        AccountManualReceipt {
            const Buggy = 0;
            var operation = 0; var legacy = 0; var cut = 0; var done = 0;
            var durable = 0; var pending = 0; var complete = 0;
            var intact = 1; var inputs = 1; var records = 1; var retired = 0;
            action Operation when (done == 0 && operation == 0) { operation = 1; }
            action Source when (done == 0 && legacy == 0) { legacy = 1; }
            action Cut when (done == 0 && 3 > cut) { cut = cut + 1; }
            action Commit when (done == 0) {
                done = 1;
                durable = if cut == 0 {0} else {1};
                pending = if cut == 1 || cut == 2 {1} else {0};
                intact = if Buggy == 1 && cut == 0 {0} else {1};
                inputs = if Buggy == 5 {0} else {1};
                records = if Buggy == 3 {0} else {1};
                complete = if cut > 0 {if Buggy == 4 {0} else {1}} else {0};
                retired = if legacy == 1 && cut > 0 {if Buggy == 2 {0} else {1}} else {0};
            }
            invariant BeforeIntentIsClean: durable == 1 || intact == 1;
            invariant InputsPreserved: inputs == 1;
            invariant AccountRecordsPreserved: records == 1;
            invariant AdmittedCompletes: done == 0 || durable == 0 || complete == 1;
            invariant RetirementFollowsIntent: (legacy == 1 && durable == 1 && retired == 1)
                || ((legacy == 0 || durable == 0) && retired == 0);
            bound ManualDomain: operation <= 1 && legacy <= 1 && cut <= 3 && done <= 1
                && durable <= 1 && pending <= 1 && complete <= 1 && intact <= 1
                && inputs <= 1 && records <= 1 && retired <= 1;
        }
    }
}
