// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Untrusted reply identity and role checks, independent of live authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_wire_reply_model() -> Model {
    aterm_spec::ty_model! {
        AccountWireReply {
            const Buggy = 0;
            // Request: claim, fence, closure, diagnostic observation.
            var request = 0;
            // Reply: claim, fence, closure, observation, unknown, denied,
            // unsupported, unavailable.
            var reply = 0;
            var parent = 1; var epoch = 1; var candidate = 1;
            var digest = 1; var phase = 1; var binding = 1;
            // Lifecycle: lost rollback, absent rollback command, compatible.
            var lifecycle = 2;
            var closure_phase = 1;
            // Successor: invalid, valid child, absent.
            var successor = 2;
            var checked = 0; var accepted = 0;
            action NextRequest when (checked == 0 && 3 > request) { request = request + 1; }
            action NextReply when (checked == 0 && 7 > reply) { reply = reply + 1; }
            action WrongParent when (checked == 0 && parent == 1) { parent = 0; }
            action WrongEpoch when (checked == 0 && epoch == 1) { epoch = 0; }
            action WrongCandidate when (checked == 0 && candidate == 1) { candidate = 0; }
            action WrongDigest when (checked == 0 && digest == 1) { digest = 0; }
            action WrongPhase when (checked == 0 && phase == 1) { phase = 0; }
            action WrongBinding when (checked == 0 && binding == 1) { binding = 0; }
            action WrongLifecycle when (checked == 0 && lifecycle == 2) { lifecycle = 0; }
            action MissingRollback when (checked == 0 && lifecycle == 2) { lifecycle = 1; }
            action WrongClosurePhase when (checked == 0 && closure_phase == 1) { closure_phase = 0; }
            action WithSuccessor when (checked == 0 && successor == 2) { successor = 1; }
            action WrongSuccessor when (checked == 0 && successor > 0) { successor = 0; }
            action Check when (checked == 0) {
                checked = 1;
                accepted = if Buggy == 10 {0} else {if Buggy == 7 && reply > 2 {1} else {
                    if (parent == 1 || Buggy == 1) && (epoch == 1 || Buggy == 2) && (digest == 1 || Buggy == 4)
                        && ((request == 0 && reply == 0) || (request == 1 && reply == 1) || (request == 2 && reply == 2) || Buggy == 6)
                        && (request == 2 || ((candidate == 1 || Buggy == 3) && (phase == 1 || Buggy == 5)))
                        && (request == 0 || request == 1 || request == 3 || ((closure_phase == 1 || Buggy == 11) && (successor > 0 || Buggy == 12)))
                        && (reply > 0 || ((binding == 1 || Buggy == 8) && (lifecycle == 2 || Buggy == 9))) {1} else {0}
                }};
            }
            invariant ExactParent: accepted == 0 || parent == 1;
            invariant ExactEpoch: accepted == 0 || epoch == 1;
            invariant ExactDigest: accepted == 0 || digest == 1;
            invariant ExactCandidate: accepted == 0 || request == 2 || candidate == 1;
            invariant ExactPhase: accepted == 0 || request == 2 || phase == 1;
            invariant MatchingRole: accepted == 0 || (request == 0 && reply == 0)
                || (request == 1 && reply == 1) || (request == 2 && reply == 2);
            invariant NoDiagnosticAck: accepted == 0 || reply <= 2;
            invariant BindingValid: accepted == 0 || reply > 0 || binding == 1;
            invariant CompatibleLifecycle: accepted == 0 || reply > 0 || lifecycle == 2;
            invariant ClosurePhaseExists: accepted == 0 || request == 0 || request == 1 || request == 3 || closure_phase == 1;
            invariant ValidSuccessor: accepted == 0 || request == 0 || request == 1 || request == 3 || successor > 0;
            invariant ExactResult: checked == 0 || accepted == (if parent == 1 && epoch == 1 && digest == 1
                && ((request == 0 && reply == 0) || (request == 1 && reply == 1) || (request == 2 && reply == 2))
                && (request == 2 || (candidate == 1 && phase == 1))
                && (request == 0 || request == 1 || request == 3 || (closure_phase == 1 && successor > 0))
                && (reply > 0 || (binding == 1 && lifecycle == 2)) {1} else {0});
            bound Finite: 0 <= request && request <= 3 && 0 <= reply && reply <= 7
                && 0 <= parent && parent <= 1 && 0 <= epoch && epoch <= 1 && 0 <= candidate && candidate <= 1
                && 0 <= digest && digest <= 1 && 0 <= phase && phase <= 1 && 0 <= binding && binding <= 1
                && 0 <= lifecycle && lifecycle <= 2 && 0 <= closure_phase && closure_phase <= 1
                && 0 <= successor && successor <= 2 && 0 <= checked && checked <= 1 && 0 <= accepted && accepted <= 1;
        }
    }
}
