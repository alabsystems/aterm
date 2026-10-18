// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;

use aterm_spec::xref::{self, SpecModule};

// The toolchain-free closure over the machines this crate binds: every anchor it writes
// resolves, every machine it binds is fully covered (its test-only seams and waivers are
// linked only here), and the Tier-1 conformances the anchors claim are run. Every other
// anchored crate's machines are aterm-models' `spec_xref_closure`. `ty --strict-vacuity`
// over the registry is `spec-check vacuity` (aterm-models), which needs the checker.

/// Where this crate's own anchors are written (`file!()` of each).
const GUI_SOURCE: &str = "crates/aterm-gui/";

/// The registered `SpecModule` set: every model linked into this binary.
fn registered_modules() -> Vec<SpecModule> {
    xref::model_registry()
        .into_iter()
        .map(SpecModule::Embedded)
        .collect()
}

/// The eleven `SelectionCustody` actions, shared by the coverage and execution-evidence
/// assertions so both check the same set.
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

/// The sixteen `PressCustody` actions — see [`SELECTION_CUSTODY_ACTIONS`].
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

/// The registered machines (as the closure names them) that some anchor or waiver linked
/// here from `source` binds.
fn machines_bound_from(report: &xref::ClosureReport, source: &str) -> BTreeSet<String> {
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.location.starts_with(source))
        .map(|r| r.machine)
        .chain(
            xref::waivers()
                .filter(|w| w.location.starts_with(source) && !w.machine.is_empty())
                .map(|w| w.machine),
        )
        .collect();
    report
        .coverage
        .iter()
        .filter(|c| {
            anchored
                .iter()
                .any(|a| xref::machine_matches(a, &c.machine))
        })
        .map(|c| c.machine.clone())
        .collect()
}

/// Assert one machine's per-step execution evidence and return the derived summary of
/// what it does and does not discriminate.
///
/// String coverage only proves each action name is claimed by some anchor, not that the
/// anchor sits on a function that runs. So the conformance opens a one-step window per
/// step (`aterm_spec::xref::StepEvidence::step`) and this requires each action's
/// anchored function to be entered by every window driving that action
/// (`StepAudit::blind_steps`), and, conversely, no anchored function to be entered by a
/// window driving an action it does not claim (`StepAudit::stray`), which discriminates
/// inside a `Terminal::process` batch.
///
/// Residual, printed in the summary: a function whose entry pattern exactly matches its
/// claimed actions is indistinguishable from the right seam (`interchangeable` is a
/// lower bound), and labels on one function can be permuted unobservably. The derived
/// figures are not a quality score.
///
/// Applied to `SelectionCustody` and `PressCustody` only; the other conformances here
/// (and anchor-bearing machines without one) have string coverage only.
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
    // Check the action set, not an anchor count: an action may legitimately have more
    // than one anchor site (e.g. `Evict` on both `set_scrollback_line_limit` and
    // `Terminal::post_process`).
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
    // The and, not the OR. `unwitnessed` above passes an action on any one of its
    // windows, so `Evict` — driven by three steps — was checked on one and unchecked
    // on two. This requires every window to have entered its own action's function,
    // which is what stops a step from quietly dropping out of the evidence by being
    // driven outside `ev.step` while a sibling step still carries the action.
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
    // The contrapositive: a function that is the seam for `{B, C}` must not be entered by
    // a window driving `D`, which catches an anchor parked on a function the batch merely
    // passes through. It can also fire for a function entered on every batch, whose entry
    // is then no evidence for its actions; the message lists the resolutions.
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

/// The stack each Tier-1 conformance body runs on ([`run_conformance_body`]). An
/// unoptimized test build moves a `WindowState` by value through every frame of
/// `BTreeMap::insert`; on 2026-09-30 input_release_pairing's `two_window_app` ->
/// `App::insert_logical_window` chain needed 2,135,360 bytes of frames, past
/// libtest's 2 MiB thread stack, and the SIGBUS at the guard page killed the whole
/// `aterm-gui --lib` binary with its tests unreported (`aterm: fatal signal 10`, as an
/// earlier test had armed crash_signal's handler). 32 MiB is reserved, not touched.
/// The bodies' stack is not what this gate judges: on 2026-10-02 the whole gate
/// still fit on one thread of 2,035,712 bytes and not of 2,023,424, about 61 KB
/// under libtest's 2 MiB, so a body's growth goes unseen here until it outgrows
/// the reserve. The by-value moves are the size; this stack only gives them room.
const CONFORMANCE_BODY_STACK: usize = 32 << 20;

/// Run one Tier-1 conformance body on a thread of its own with
/// [`CONFORMANCE_BODY_STACK`], recording a panic as `machine: message` in `failed`, so
/// one red body does not hide a later one and the gate names every failure. The thread
/// is joined before this returns, so a body's thread-local evidence windows
/// (`aterm_spec::xref`) open and close on that one thread.
fn run_conformance_body<T: Send>(
    machine: &str,
    failed: &mut Vec<String>,
    body: impl FnOnce() -> T + Send,
) -> Option<T> {
    let joined = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(format!("spec_xref_closure/{machine}"))
            .stack_size(CONFORMANCE_BODY_STACK)
            .spawn_scoped(scope, body)
            .map(|thread| thread.join())
    });
    match joined {
        Ok(Ok(value)) => Some(value),
        Ok(Err(payload)) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            failed.push(format!("{machine}: {message}"));
            None
        }
        Err(spawn) => {
            failed.push(format!("{machine}: its thread did not start: {spawn}"));
            None
        }
    }
}

#[test]
fn spec_xref_closure() {
    // ---- Proof #1: the registry ACTUALLY collected the anchors, cross-crate. ----
    let refs: Vec<_> = xref::refinements().collect();
    let waivers: Vec<_> = xref::waivers().collect();

    // Per-machine collected counts (the cross-crate collection evidence).
    let machines: BTreeSet<&str> = refs
        .iter()
        .map(|r| r.machine)
        .chain(
            waivers
                .iter()
                .filter(|w| !w.machine.is_empty())
                .map(|w| w.machine),
        )
        .collect();
    crate::logging::stderr_line!(
        "spec_xref_gate: collected {} refinement anchor(s) + \
         {} waiver(s) across {} machine(s):",
        refs.len(),
        waivers.len(),
        machines.len()
    );
    for m in &machines {
        let r = refs.iter().filter(|x| x.machine == *m).count();
        let w = waivers.iter().filter(|x| x.machine == *m).count();
        crate::logging::stderr_line!("    {m:<16} refinements={r:<3} waivers={w}");
    }

    // ---- Build the registered SpecModule set ----
    let modules = registered_modules();
    crate::logging::stderr_line!("spec_xref_closure: {} registered model(s)", modules.len());

    // ---- Run the obligations (1, 3, 4), scoped to what this crate binds ----
    // Every other anchored crate's machines are closed by aterm-models'
    // `spec_xref_closure`, which links them all from outside; a machine this crate binds is
    // closed here, where its test-only seams and waivers are linked too.
    let mut report = xref::check_closure(&modules);
    let ours = machines_bound_from(&report, GUI_SOURCE);
    report.violations.retain(|v| {
        if v.obligation == 3 {
            ours.iter()
                .any(|m| v.message.starts_with(&format!("machine `{m}` ")))
        } else {
            v.message.contains(GUI_SOURCE)
        }
    });
    report.coverage.retain(|c| ours.contains(&c.machine));

    crate::logging::stderr_line!("spec_xref_closure: per-machine coverage ledger:");
    for c in &report.coverage {
        crate::logging::stderr_line!(
            "  {:<16} ratio={:.3} bound={:<3} waived={:<3} actions={:<3} {}{}",
            c.machine,
            c.ratio(),
            c.bound.len(),
            c.waived.len(),
            c.total_actions,
            if c.active {
                "[ACTIVE]"
            } else {
                "[report-only]"
            },
            if c.active && !c.uncovered.is_empty() {
                format!(" UNCOVERED={:?}", c.uncovered)
            } else {
                String::new()
            },
        );
    }

    // ---- Proof #2: GREEN (no obligation violations) ----
    assert!(
        report.is_ok(),
        "spec_xref_closure FAILED — source↔spec obligations violated:\n{}",
        report
            .violations
            .iter()
            .map(|v| format!("  [obligation {}] {}", v.obligation, v.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Each Tier-1 body runs under its own catch (`run_conformance_body`) and the
    // failures are asserted together once the last body has run.
    let mut failed_bodies: Vec<String> = Vec::new();
    // Proof #3a: `window_routing` is actively bound (the `#[refines]` on
    // `insert_logical_window` / `close_window_logical`); assert that, then run its real
    // Tier-1 conformance here so the gate fails if close→exit routing diverges.
    {
        let wr = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("window_routing", &c.machine))
            .expect("window_routing must be in the coverage ledger");
        assert!(
            wr.active,
            "window_routing must be ACTIVELY-BOUND (>= 1 refinement on the real App seams) — \
             finding 3 requires it be gated, not report-only"
        );
        assert_eq!(
            wr.ratio(),
            1.0,
            "window_routing must be fully bound (CreateWindow + CloseWindow + \
             ConnectedCreateWindow + RaiseSession all anchored); uncovered = {:?}",
            wr.uncovered
        );
    }
    // Run it (the gate now proves window_routing Tier-1, not merely claims it).
    if run_conformance_body(
        "window_routing",
        &mut failed_bodies,
        super::window_routing_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: window_routing is actively-bound AND its Tier-1 conformance \
             (real App create/close→exit routing) was RUN by the gate (finding 3)."
        );
    }

    // `pane_tree` is actively bound via `PaneTree::split_focused` / `close_pane`; run the
    // real split/close conformance here.
    {
        let pt = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("pane_tree", &c.machine))
            .expect("pane_tree must be in the coverage ledger");
        assert!(
            pt.active,
            "pane_tree must be ACTIVELY-BOUND (>= 1 refinement on the real PaneTree seams)"
        );
        assert_eq!(
            pt.ratio(),
            1.0,
            "pane_tree must be fully bound (Split + Close both anchored); uncovered = {:?}",
            pt.uncovered
        );
    }
    if run_conformance_body(
        "pane_tree",
        &mut failed_bodies,
        super::pane_tree_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: pane_tree is actively-bound AND its Tier-1 conformance \
             (real split/close re-point) was RUN by the gate."
        );
    }

    // `session_pool` is actively bound via `SessionPool::attach` / `detach`; run the real
    // refcount conformance here.
    {
        let sp = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("session_pool", &c.machine))
            .expect("session_pool must be in the coverage ledger");
        assert!(
            sp.active,
            "session_pool must be ACTIVELY-BOUND (>= 1 refinement on the real pool seams)"
        );
        assert_eq!(
            sp.ratio(),
            1.0,
            "session_pool must be fully bound (Acquire + Release both anchored); uncovered = {:?}",
            sp.uncovered
        );
    }
    if run_conformance_body(
        "session_pool",
        &mut failed_bodies,
        super::session_pool_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: session_pool is actively-bound AND its Tier-1 conformance \
             (real attach/detach refcount accounting) was RUN by the gate."
        );
    }

    // The `#[refines]` on the real `open_tab_in` / `cycle_tab` / `close_tab_at`
    // seams make `tab_strip` an active, coverage-gated machine; assert that, THEN
    // drive the real NewTab/SelectTab/Close conformance — incl. the non-front close
    // whose stale-strip desync `StripMirrorsTruth` forbids — from inside the gate.
    {
        let ts = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("tab_strip", &c.machine))
            .expect("tab_strip must be in the coverage ledger");
        assert!(
            ts.active,
            "tab_strip must be ACTIVELY-BOUND (>= 1 refinement on the real tab seams)"
        );
        assert_eq!(
            ts.ratio(),
            1.0,
            "tab_strip must be fully bound (NewTab + SelectTab + Close all anchored); uncovered = {:?}",
            ts.uncovered
        );
    }
    if run_conformance_body(
        "tab_strip",
        &mut failed_bodies,
        super::tab_strip_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: tab_strip is actively-bound AND its Tier-1 conformance \
             (native strip parity, incl. a non-front-window close) was RUN by the gate."
        );
    }

    // The asymmetric layout machine spans three real presentation layers:
    // Renderer/WindowCpu, GpuRenderer/WindowGpu, and this GUI RepaintKey.
    // Cross-crate anchors make the whole five-action machine active; run the
    // GUI decision conformance here so coverage cannot be a decorative ledger.
    {
        let layout = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("AsymmetricPadLayout", &c.machine))
            .expect("AsymmetricPadLayout must be in the coverage ledger");
        assert!(
            layout.active,
            "AsymmetricPadLayout must be ACTIVELY-BOUND by CPU/GPU/GUI layout seams"
        );
        assert_eq!(
            layout.ratio(),
            1.0,
            "AsymmetricPadLayout must be fully bound-or-waived; uncovered = {:?}",
            layout.uncovered
        );
        for action in [
            "ApplyInitialTop",
            "PrimeLayoutCache",
            "ApplyChangedTop",
            "RenderWithLayoutCache",
        ] {
            assert!(
                layout.bound.contains(action),
                "shipping layout/cache action `{action}` must carry a #[refines] anchor"
            );
        }
        assert!(
            layout.waived.contains("PickLayout"),
            "only the model's nondeterministic external input selection may be waived"
        );
    }
    if run_conformance_body(
        "AsymmetricPadLayout",
        &mut failed_bodies,
        super::early_out_tests::run_asymmetric_pad_layout_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: AsymmetricPadLayout is fully bound across CPU/GPU/GUI and the \
             real RepaintKey origin-change conformance was RUN by the gate."
        );
    }

    // Cross-window key ownership is active only when the twelve owner/routing seams are
    // linked: require all 21 actions bound or waived, then run the two-window
    // conformance.
    {
        let pairing = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("input_release_pairing", &c.machine))
            .expect("input_release_pairing must be in the coverage ledger");
        assert!(
            pairing.active,
            "input_release_pairing must be ACTIVELY-BOUND by the physical owner-map seams"
        );
        assert_eq!(
            pairing.ratio(),
            1.0,
            "input_release_pairing must be fully bound-or-waived; uncovered = {:?}",
            pairing.uncovered
        );
        for action in [
            "ConsumePhysicalPress",
            "ForwardPress",
            "ForwardLiteralPress",
            "CaptureLocalRepeatPress",
            "SwallowUntrackedRepeat",
            "ForwardRepeatOfForwardedPress",
            "ForwardRepeatOfLiteralPress",
            "ForwardLocalRepeat",
            "ReleaseConsumedPress",
            "ReleaseForwardedPress",
            "ReleaseLiteralPress",
            "ReleaseLocalRepeatPress",
        ] {
            assert!(
                pairing.bound.contains(action),
                "real physical-owner action `{action}` must carry a #[refines] anchor"
            );
        }
        assert!(
            pairing.waived.contains("TransferFocusWhileHeld"),
            "focus transfer is an explicit OS-environment waiver, never an uncovered action"
        );
    }
    if run_conformance_body(
        "input_release_pairing",
        &mut failed_bodies,
        super::app_input::input_release_pairing_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: input_release_pairing is actively-bound AND its Tier-1 \
             two-window owner-map conformance was RUN by the gate."
        );
    }

    // Selection custody: anchors on the gesture/press seams (`App::finish_selection`,
    // `app_input::apply_press_custody`) and engine seams (`Terminal::post_process`,
    // `set_scrollback_line_limit`, `clear_scrollback`) make it active. Assert all eleven
    // actions bound and none waived, then run the Tier-1 body.
    {
        let custody = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("SelectionCustody", &c.machine))
            .expect("SelectionCustody must be in the coverage ledger");
        assert!(
            custody.active,
            "SelectionCustody must be ACTIVELY-BOUND (>= 1 refinement on the real \
             selection seams), not report-only"
        );
        assert_eq!(
            custody.total_actions,
            SELECTION_CUSTODY_ACTIONS.len(),
            "SelectionCustody action inventory drifted; update the anchors AND the \
             Tier-1 conformance, found {}",
            custody.total_actions
        );
        assert_eq!(
            custody.ratio(),
            1.0,
            "SelectionCustody must be fully bound-or-waived; uncovered = {:?}",
            custody.uncovered
        );
        for action in SELECTION_CUSTODY_ACTIONS {
            assert!(
                custody.bound.contains(*action),
                "SelectionCustody action `{action}` must carry a #[refines] anchor on a \
                 REAL seam (it is bound, not waived: every one of the eleven has a site)"
            );
        }
        assert!(
            custody.waived.is_empty(),
            "SelectionCustody carries no waivers — every action has an observable \
             shipping site; found {:?}",
            custody.waived
        );
    }
    // Execution evidence: `run_conformance` drives each step in its own window and
    // returns the ledger; `assert_step_evidence` checks each action's function was
    // entered by every window driving it and by none driving an action it does not
    // claim.
    let sel_ev = run_conformance_body(
        "SelectionCustody",
        &mut failed_bodies,
        super::selection_custody_conformance::run_conformance,
    );

    // Press custody: anchors sit on the four recorders that write `Terminal::note_custody`
    // (`app_input::note_press_custody`, `app_mouse::note_selection_custody`,
    // `Terminal::note_scroll_custody` inside the primitives that raise `display_offset`,
    // and `Terminal::note_output_custody`). They must differ from `SelectionCustody`'s
    // seams because the `#[refines]` const name omits the machine, so same-named
    // actions cannot share a function. Recording which transition fired is what
    // distinguishes actions that are otherwise identical in every observable, and it
    // backs the `custody` control verb.
    {
        let press = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("PressCustody", &c.machine))
            .expect("PressCustody must be in the coverage ledger");
        assert!(
            press.active,
            "PressCustody must be ACTIVELY-BOUND (>= 1 refinement on the real custody \
             recorders), not report-only"
        );
        assert_eq!(
            press.total_actions,
            PRESS_CUSTODY_ACTIONS.len(),
            "PressCustody action inventory drifted; update the anchors AND the Tier-1 \
             conformance, found {}",
            press.total_actions
        );
        assert_eq!(
            press.ratio(),
            1.0,
            "PressCustody must be fully bound-or-waived; uncovered = {:?}",
            press.uncovered
        );
        for action in PRESS_CUSTODY_ACTIONS {
            assert!(
                press.bound.contains(*action),
                "PressCustody action `{action}` must carry a #[refines] anchor on the \
                 REAL recorder (it is bound, not waived: the custody record gives every \
                 one of the sixteen an observable site)"
            );
        }
        assert!(
            press.waived.is_empty(),
            "PressCustody carries no waivers — `OutputAtLive` is anchored and DRIVEN \
             rather than waived, because the record makes its transition nameable at \
             all: a batch that destroyed the highlight it was not entitled to \
             destroy comes back under a DIFFERENT variant, so the step fails on the \
             recorded name; found {:?}",
            press.waived
        );
    }
    // Execution evidence as for SelectionCustody: pins that the recorders are the
    // functions each transition's step actually reaches.
    let press_ev = run_conformance_body(
        "PressCustody",
        &mut failed_bodies,
        super::press_custody_conformance::run_conformance,
    );
    // Every body has run under its own catch; name all failures.
    assert!(
        failed_bodies.is_empty(),
        "spec_xref_closure: {} Tier-1 conformance bod(ies) failed (a later entry can \
         be a consequence of an earlier one; `--ignored --exact <wrapper>` runs one \
         alone):\n{}",
        failed_bodies.len(),
        failed_bodies.join("\n")
    );
    let (Some(sel_ev), Some(press_ev)) = (sel_ev, press_ev) else {
        unreachable!("a body that returned no ledger is listed in failed_bodies")
    };
    // SelectionCustody's execution evidence (its ledger was taken above).
    let sel_summary = assert_step_evidence("SelectionCustody", SELECTION_CUSTODY_ACTIONS, &sel_ev);
    crate::logging::stderr_line!(
        "spec_xref_closure: SelectionCustody is actively-bound (11/11 actions, zero \
         waivers) AND its Tier-1 conformance (real drag/press gestures + real VT \
         damage/scroll/eviction/ED-3/RIS batches) was RUN by the gate — with each \
         action proved to have entered AT LEAST ONE of its annotated functions in \
         EVERY STEP WINDOW THAT DRIVES IT, not merely somewhere in the run. (\"At \
         least one\": the design permits several anchor sites per action, and the \
         check is an OR across them, so a second site is never required to run — the \
         count of sites that never did is in the derived numbers.) THE EXACT CLAIM, \
         in two halves, and no more than the two. FIRST, entry: an anchor moved onto \
         a function this action's own windows do NOT enter fails by name — a stub \
         with no callers in the tree, or a function only some other step calls. That \
         half alone would be weak, because a window is only as narrow as the seam its \
         step calls and a `Terminal::process` batch enters several anchored functions \
         at once. SECOND, and this is what narrows it inside one window: an anchored \
         function must NOT be entered by a window driving an action it is not \
         anchored for. `SelectionCustody::UniformScroll` parked on \
         `Terminal::note_output_custody` — a function that same batch really does \
         enter — fails on that second half, because it also runs in the damage and \
         invalidation windows. WHAT SURVIVES BOTH: a move onto a function whose entry \
         pattern is EXACTLY the action set it claims (including any unanchored \
         function inside the window, which runs no probe and so cannot even be \
         counted below), and any permutation of the actions that share one function. \
         {sel_summary} \
         Still unproved, and WIDER than the sentence above: this audit's whole \
         notion of a \"function\" is the bare `fn` IDENTIFIER the macro captured, so \
         two distinct functions that happen to share a name are ONE node here and a \
         move between them is invisible — the entry-pattern check cannot separate \
         what the identifier already merged. Beyond that the `project=` and \
         `rust_method` paths remain unresolved strings no layer maps to a symbol \
         (obligation 2 proper — Phase 3 `trust-ir`). The conformance remains the half that \
         carries real weight: it drives shipping code and is falsified by a \
         regression in it."
    );
    let press_summary = assert_step_evidence("PressCustody", PRESS_CUSTODY_ACTIONS, &press_ev);
    crate::logging::stderr_line!(
        "spec_xref_closure: PressCustody is actively-bound (16/16 actions, zero \
         waivers) AND its Tier-1 conformance (real scroll/wheel/gesture/press seams — \
         all four press classes delivered as real key events — plus real \
         `Terminal::process` batches, every step named by the ENGINE's own custody \
         record rather than by the harness) was RUN by the gate, each step inside its \
         own execution-evidence window. As with SelectionCustody the coverage half is \
         no longer only a claim about anchor STRINGS — each action's annotated \
         function (or, where an action has several anchor sites, at least one of \
         them) is proved to have been entered by EVERY window that drives it AND to \
         be entered by no window driving an action it is not anchored for AMONG THIS \
         MACHINE'S OWN ANCHORS — the scope matters, because a function carrying \
         anchors for a DIFFERENT machine is invisible to this audit, and the \
         shared-function line below prints the counterexample from inside this same \
         ledger — the same two halves as above, and with the same residual: a function whose entry \
         pattern is exactly the action set it claims is indistinguishable from the \
         right seam, and so is any permutation of the actions sharing one function. \
         Per-branch attribution and \
         `project=` symbol resolution remain unproved: {press_summary} The \
         conformance is still the half that drives shipping code and goes red on a \
         regression in it. Its per-step verdict is a REFINEMENT check (the action's \
         guard and its exact update image) PLUS an evaluation of every model \
         invariant over the observed pre- and post-states; whole-reachable-space \
         invariant checking is Tier-0's, in `aterm-models/tests/derived_ring_ty.rs`."
    );

    crate::logging::stderr_line!(
        "spec_xref_gate: LOCAL GREEN — obligations 1/3/4 hold for the {} machine(s) this crate \
         binds; Tier-1 conformances discharged by the in-process interpreter.",
        ours.len()
    );
}
