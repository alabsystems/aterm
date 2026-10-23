// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;

const SELECTION_CUSTODY_ACTIONS: &[&str] = &[
    "SelectLow",
    "SelectOldest",
    "SelectHigh",
    "UserClear",
    "TypingPress",
    "InertPress",
    "UniformScroll",
    "RegionDamageLow",
    "RegionDamageHigh",
    "Evict",
    "WholesaleInvalidate",
];

const PRESS_CUSTODY_ACTIONS: &[&str] = &[
    "UserScroll",
    "UserScrollTowardLive",
    "SnapToLive",
    "UserSelect",
    "UserClear",
    "TypingPress",
    "RepeatPress",
    "InertPress",
    "ReleaseEvent",
    "OutputAtLive",
    "OutputWhileReading",
    "OutputDamagesTheSelectedRows",
    "OutputDamagesTheSelectedRowsInPlace",
    "OutputTookTheSelectionUnattributed",
    "OutputTookTheSelectionUnattributedInPlace",
    "OutputInvalidatesTheCoordinateSpace",
];

fn assert_step_evidence(
    machine: &str,
    expected_actions: &[&str],
    ev: &aterm_spec::xref::StepEvidence,
) -> String {
    let audit = ev.audit();
    assert!(
        audit.uninstrumented.is_empty(),
        "{machine}: every anchor must carry an entry probe — an anchor the macro \
         could not instrument (a `const fn`, or a non-fn item) would be a way back \
         out of this check; unprobed: {:?}",
        audit.uninstrumented
    );
    assert!(
        audit.ambiguous.is_empty(),
        "{machine}: two anchors share an entry id, so one fn's entry would discharge \
         the other's obligation: {:?}",
        audit.ambiguous
    );
    let want: BTreeSet<&str> = expected_actions.iter().copied().collect();
    let got: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(
        got,
        want,
        "{machine}: the ACTIONS carried by anchors linked into this gate's binary \
         must be exactly the {} this gate checks. Missing {:?}; unexpected {:?}. \
         (Anchor SITES may be many-per-action and are not counted here: {} sites \
         across {} functions.)",
        expected_actions.len(),
        want.difference(&got).collect::<Vec<_>>(),
        got.difference(&want).collect::<Vec<_>>(),
        audit.anchors,
        audit.by_function.len(),
    );
    assert!(
        audit.unanchored_steps.is_empty(),
        "{machine}: the conformance tagged step(s) with action name(s) no anchor of \
         this machine carries, so those steps checked nothing: {:?}",
        audit.unanchored_steps
    );
    assert!(
        audit.undriven.is_empty(),
        "{machine}: anchored action(s) that the Tier-1 conformance never DROVE, so \
         no step window could witness them: {:?}. Drive them, or the anchor is a \
         claim nothing tests.",
        audit.undriven
    );
    assert!(
        audit.unwitnessed.is_empty(),
        "{machine}: anchored action(s) whose annotated function was NOT ENTERED by \
         the step that drives them: {:?}. The conformance exercises the action, so \
         the function the anchor sits on is not the function that performs it — move \
         the anchor to the real seam (or drive that seam). Note that entry ELSEWHERE \
         in the run does not count: the window is one step wide, which is what makes \
         an anchor moved onto a function OUTSIDE this action's own window fail here. \
         Witnessed {}/{} step windows.",
        audit.unwitnessed,
        audit.witnessed_steps,
        audit.steps,
    );
    assert!(
        audit.blind_steps.is_empty(),
        "{machine}: step window(s) that did NOT enter their own action's annotated \
         function: {:?}. An action driven by several steps is `witnessed` on one of \
         them, so these would otherwise check nothing at all — either the seam that \
         step calls is not the anchored one, or the step was driven outside its \
         window. Witnessed {}/{} step windows.",
        audit.blind_steps,
        audit.witnessed_steps,
        audit.steps,
    );
    assert!(
        audit.stray.is_empty(),
        "{machine}: anchored function(s) entered by the window of an action they are \
         NOT anchored for: {:?} (fn -> the foreign actions whose windows entered it). \
         Entry in such a function is not evidence for the actions it claims, because \
         it also runs when those other actions fire. Either the anchor is on the \
         wrong function (move it to the seam that is specific to its action), or the \
         function really is the shared seam for those actions too (anchor it for them \
         as well), or the step's window is wider than the seam it means to name \
         (narrow it).",
        audit.stray
    );
    assert!(
        audit.is_complete(),
        "{machine} step-evidence audit incomplete: {audit:?}"
    );
    audit.summary()
}

#[test]
fn selection_custody_steps_enter_their_own_seams() {
    let ev = crate::conformance_support::on_conformance_stack(
        "SelectionCustody",
        super::selection_custody_conformance::run_conformance,
    );
    let summary = assert_step_evidence("SelectionCustody", SELECTION_CUSTODY_ACTIONS, &ev);
    crate::logging::stderr_line!(
        "spec_xref_gate: SelectionCustody's Tier-1 ran with each step in its own evidence \
         window: {summary} Residual: a function whose entry pattern is exactly the actions \
         it claims is indistinguishable from the right seam, and a function is its bare \
         identifier here."
    );
}

#[test]
fn press_custody_steps_enter_their_own_seams() {
    let ev = crate::conformance_support::on_conformance_stack(
        "PressCustody",
        super::press_custody_conformance::run_conformance,
    );
    let summary = assert_step_evidence("PressCustody", PRESS_CUSTODY_ACTIONS, &ev);
    crate::logging::stderr_line!(
        "spec_xref_gate: PressCustody's Tier-1 ran with each step in its own evidence \
         window: {summary} Residual: as for SelectionCustody; a function carrying another \
         machine's anchors is invisible to this audit."
    );
}
