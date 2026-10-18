// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The anchor half of the source↔spec cross-reference: the records the
//! `#[refines]` and `#[spec_unmodeled]` attributes submit,
//! and the per-thread record of which anchored functions ran.
//!
//! The spec half (the model registry, the closure, the per-step audit) is
//! `aterm_spec::xref`, which re-exports everything here.

use std::collections::BTreeSet;

/// A source→spec refinement binding, emitted by `#[refines(machine, action, …)]`.
///
/// This is the registry-collectable anchor record. Its fields are `&'static str`
/// so it can live in a `static` submitted at load time (the macro span-captures
/// `file!()`/`line!()`).
#[derive(Debug, Clone, Copy)]
pub struct RefinementAnchor {
    /// The TLA+ machine name (e.g. `"terminal_modes"`) — matches a `Model::name`
    /// (lower/CamelCase resolved by `aterm_spec::xref::machine_matches`) or an external module name.
    pub machine: &'static str,
    /// The TLA+ action name (e.g. `"SetCursorVisible"`) — matches an `Action`.
    pub action: &'static str,
    /// The annotated Rust fn (path/name), e.g. `"TerminalHandler::show_cursor"`.
    pub rust_method: &'static str,
    /// Source location `file:line` (proc-macro `file!()`/`line!()`).
    pub location: &'static str,
    /// The projection fn path (the `project=` arg, which `#[refines]`
    /// requires): what Tier-1 reads the real state through.
    pub project: &'static str,
    /// The EXECUTION-EVIDENCE key for this anchor: the id the `#[refines]` macro
    /// also injects as a fn-entry probe into the annotated body, so
    /// [`entered_anchor_ids`] can answer "did the annotated function actually RUN?".
    ///
    /// Shape `"<machine>::<action> @ <rust_method>"`, emitted from the SAME macro
    /// expansion as the probe, so the two strings cannot drift.
    ///
    /// `""` means NOT INSTRUMENTED — the macro could not inject a probe (a `const fn`
    /// body, or an item that is not a fn with a block). An empty `entry_id` is not a
    /// pass: a gate that demands execution evidence must treat it as a failure (see
    /// `aterm_spec::xref::StepAudit::uninstrumented`), otherwise moving an anchor onto a `const fn`
    /// would be a way to opt back out of the check.
    pub entry_id: &'static str,
}

/// An explicit "this fn is intentionally NOT modeled" waiver, emitted by
/// `#[spec_unmodeled(reason, …)]`. `machine`/`action` are optional (a bare
/// `reason="…"` waiver — the legacy bypass-setter form — leaves them `""`); when
/// present, the waiver discharges that model `Action` for the coverage obligation.
#[derive(Debug, Clone, Copy)]
pub struct Waiver {
    pub machine: &'static str,
    pub action: &'static str,
    pub reason: &'static str,
    pub rust_method: &'static str,
    pub location: &'static str,
}

crate::registry::collect!(RefinementAnchor);
crate::registry::collect!(Waiver);

/// All [`RefinementAnchor`]s linked into the current binary.
pub fn refinements() -> impl Iterator<Item = &'static RefinementAnchor> {
    crate::registry::iter::<RefinementAnchor>()
}

/// All [`Waiver`]s linked into the current binary.
pub fn waivers() -> impl Iterator<Item = &'static Waiver> {
    crate::registry::iter::<Waiver>()
}

// ---------------------------------------------------------------------------
// EXECUTION EVIDENCE — the runnable fragment of obligation 2.
// ---------------------------------------------------------------------------
//
// THE HOLE THIS ADDRESSES. A `RefinementAnchor` is submitted by an attribute that
// leaves the annotated fn untouched, and `check_closure` only ever compared
// STRINGS: that the anchor's `action` names a real action of a real machine, and
// that every action of an active machine is named by some anchor. NOTHING tied the
// anchor to a fn that does the thing. Demonstrated, not theorised: moving a
// `SelectionCustody` anchor onto `Terminal::is_tmux_mode_active()` — a fn (since
// deleted) whose entire body was `false`, with zero callers — left the gate GREEN at
// `ratio=1.000 bound=11 waived=0 [ACTIVE]`.
//
// THE MECHANISM. In `cfg(any(test, debug_assertions))` builds — the cfg the anchor
// ATTRIBUTE is under — the macro ALSO injects, as the first statement of the
// annotated fn's body, a call to [`note_entered`] with the anchor's
// [`RefinementAnchor::entry_id`].
//
// WHAT THAT CFG GATES, AND WHAT IT DOES NOT. The cfg gates the ATTRIBUTE, hence the
// probe CALL SITES: a release build of `aterm-core` (no `debug_assertions`, not
// `cfg(test)`) has no `note_entered` call in `write_char`. It does NOT gate this
// module, which every anchored crate depends on unconditionally: the two
// thread-local statics below and the `pub fn`s over them carry no `#[cfg]`, because
// the cfg that strips a probe is the ANNOTATED crate's, and this crate cannot know
// at its own compile time whether some downstream crate will emit a call. With no
// call site the linker drops them; what the cfg buys is the COST paragraph below:
// no release code path pays even the `bool` load.
//
// PER STEP, NOT PER RUN — and that distinction is the whole strength of this. One window around the
// WHOLE conformance asks only "was this fn entered at some point during a run that drives eleven
// actions". That is far weaker than it reads: an anchor moved onto ANY fn the conformance happens
// to touch — `Terminal::process`, or even `Terminal::text_selection`, which only the HARNESS'S
// projection calls — is entered during the run and passes. So the window is ONE STEP wide:
// `aterm_spec::xref::StepEvidence::step` clears the record, runs the single shipping call that
// drives action A, and
// snapshots what was entered before the harness reads anything back. `Evict`'s anchor moved onto
// `Terminal::process` then fails, because the `Evict` step calls only `set_scrollback_line_limit`.
//
// AND THE CONTRAPOSITIVE, because per-step entry alone is still weaker than it reads.
// A window is only as narrow as the seam its step calls, and a step that drives a
// `Terminal::process` batch runs the whole VT engine: several anchored functions are
// entered at once and entry cannot tell them apart. `SelectionCustody`'s uniform-scroll
// window enters `Terminal::note_output_custody` as well as `Terminal::post_process`, so
// entry alone would let the anchor sit on either. What separates them is asking the
// other question — a function that is the seam for `{B, C}` must NOT be entered by a
// window driving `D` — and `note_output_custody` also runs in the damage and
// invalidation windows. `aterm_spec::xref::StepAudit::stray` is that check; `aterm_spec::xref::StepAudit::interchangeable`
// is what entry alone would have left ambiguous, printed so the difference is visible
// rather than asserted.
//
// THREAD-LOCAL, deliberately. The record is per-thread, so a step's window sees ONLY
// what that step called on the gate's own thread. A process-wide set would be silently
// satisfied by any other test running in parallel in the same binary — which is
// exactly the false green this is here to remove. A window whose work runs on threads
// of its own (a conformance replaying its paths on a pool) gets their entries only by
// hand: each worker opens a window, [`take_entered_anchors`] closes it and hands its
// record back, and [`merge_entered_anchors`] credits the owner's open window with it.
//
// ARMED, deliberately. The probe is injected into ~190 bodies tree-wide, several of
// which are per-character hot paths (`Terminal::write_char`). Outside a step window the
// probe is a single thread-local `bool` load and a predictable branch:
// `aterm_spec::xref::StepEvidence::step` arms on entry and disarms on exit, and nothing else arms.
//
// WHAT IT IS NOT, stated plainly because the arithmetic matters more than the
// adjective. This is FUNCTION-ENTRY evidence, not per-action BRANCH evidence. A fn
// carrying four anchors marks all four the moment it is entered once, so for those
// four actions the evidence is "the annotated function ran during this action's own
// step" and NOT "the branch that implements this action was taken". Both
// `Terminal::post_process` (4 `SelectionCustody` actions) and
// `Terminal::note_output_custody` (7 `PressCustody` actions) are entered
// unconditionally on every VT batch, so nothing here can tell their siblings apart at
// all — permuting either function's labels among themselves (its four, its seven)
// changes no observable.
// `aterm_spec::xref::StepAudit::summary` DERIVES that count and prints it rather than letting the gate
// claim more; `aterm_spec::xref::StepAudit::shared_site` is the anchor-table list and
// `aterm_spec::xref::StepAudit::indiscriminate` the run-derived one. The other surviving hole is an
// anchor moved onto a function whose ENTRY PATTERN is exactly the action set it claims
// — including any unanchored function inside the window, which runs no probe and so
// cannot even be counted. Full symbol and branch resolution is still Phase 3's
// `trust-ir` job.

thread_local! {
    /// Is a `aterm_spec::xref::StepEvidence::step` window open on this thread? The probe's first act
    /// is to read this and return, so an un-armed probe costs one TLS `bool` load.
    /// `const`-initialized, so the slot needs no lazy-init check.
    static ARMED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };

    /// Anchor ids whose annotated fn was entered on THIS thread inside the open
    /// window. `BTreeSet` (not `HashSet`) so the initializer is `const` — the TLS
    /// costs nothing until the first probe fires.
    static ENTERED_ANCHORS: core::cell::RefCell<BTreeSet<&'static str>> =
        const { core::cell::RefCell::new(BTreeSet::new()) };
}

/// Record that the fn carrying `entry_id` was ENTERED on this thread.
///
/// Called by the probe the `#[refines]` macro injects at fn entry; never called by
/// hand. Two properties it must have, because it is injected into ~190 bodies:
///
/// * NEARLY FREE when no gate is watching. The `ARMED` read short-circuits before any
///   `RefCell` borrow or `BTreeSet` insert, so the cost outside a step window is a
///   thread-local `bool` load and a branch the predictor gets right every time.
/// * PANIC-FREE by construction — it must be safe to inject into ANY annotated fn,
///   including one that runs during TLS teardown or re-entrantly:
///   [`std::thread::LocalKey::try_with`] tolerates a destroyed TLS slot and
///   `try_borrow_mut` tolerates a re-entrant call, and both simply drop the record
///   rather than unwinding inside somebody else's function.
// Skip: thread-local bookkeeping (`LocalKey::try_with`, `RefCell::try_borrow_mut`,
// `BTreeSet::insert` — absent std bodies). Verification tooling, like the rest of
// this module.
#[cfg_attr(trust_verify, trust::skip)]
#[inline]
pub fn note_entered(entry_id: &'static str) {
    if !ARMED.try_with(|armed| armed.get()).unwrap_or(false) {
        return;
    }
    let _ = ENTERED_ANCHORS.try_with(|cell| {
        if let Ok(mut set) = cell.try_borrow_mut() {
            set.insert(entry_id);
        }
    });
}

/// Open an evidence window: forget every anchor entry recorded on this thread and ARM
/// the probe. Returns whether the window is genuinely open — the record is provably
/// empty AND the probe armed.
///
/// THE RETURN VALUE IS THE POINT. Both failure modes (`try_with` on a destroyed TLS
/// slot, `try_borrow_mut` on a re-entrant borrow) would otherwise leave the PREVIOUS
/// conformance's entries in place, and every audit after it would pass on stale
/// evidence. A caller that cannot open the window must fail loudly;
/// `aterm_spec::xref::StepEvidence::step` asserts on it.
// Skip: thread-local bookkeeping (`LocalKey::try_with`, `RefCell::try_borrow_mut`,
// `BTreeSet::clear/is_empty` — absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn reset_entered_anchors() -> bool {
    let cleared = ENTERED_ANCHORS
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut set) => {
                set.clear();
                set.is_empty()
            }
            Err(_) => false,
        })
        .unwrap_or(false);
    let armed = ARMED
        .try_with(|armed| {
            armed.set(true);
            armed.get()
        })
        .unwrap_or(false);
    cleared && armed
}

/// Close the evidence window: the probe goes back to being one `bool` load.
///
/// Idempotent, and never fails loudly — a thread whose TLS is already gone has no
/// window to close. The recorded ids are left alone so a caller can still read them.
// Skip: thread-local bookkeeping (`LocalKey::try_with`, `Cell::set`). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn disarm_entered_anchors() {
    let _ = ARMED.try_with(|armed| armed.set(false));
}

/// The anchor ids entered on this thread since the last [`reset_entered_anchors`].
///
/// LOSSY ON FAILURE, deliberately, and [`entered_anchor_count`] exists because of it:
/// a destroyed TLS slot and an outstanding borrow both come back as an EMPTY set,
/// indistinguishable from a genuinely empty record. That is the right shape for a
/// reader that wants "what did this window see" and the wrong shape for an assertion
/// about emptiness, which would then pass in exactly the two states it claims to
/// detect.
// Skip: thread-local read + `BTreeSet::clone` (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn entered_anchor_ids() -> BTreeSet<&'static str> {
    ENTERED_ANCHORS
        .try_with(|cell| match cell.try_borrow() {
            Ok(set) => (*set).clone(),
            Err(_) => BTreeSet::new(),
        })
        .unwrap_or_default()
}

/// How many ids are recorded on this thread — `None` when the record could not be
/// READ at all (destroyed TLS slot, or an outstanding borrow).
///
/// THE DISTINCTION IS THE WHOLE POINT. `entered_anchor_ids().is_empty()` folds
/// "the record is clean" together with "I could not look", so an assertion written on
/// it is vacuous: it holds precisely when the state it guards against is unobservable.
/// `Some(0)` is a POSITIVE observation of an empty record and `None` is a failure to
/// observe, so `aterm_spec::xref::StepEvidence::step`'s post-reset check can be genuinely independent
/// of [`reset_entered_anchors`]'s own return value instead of merely echoing it.
// Skip: thread-local read + `RefCell::try_borrow` (absent std bodies). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn entered_anchor_count() -> Option<usize> {
    ENTERED_ANCHORS
        .try_with(|cell| cell.try_borrow().ok().map(|set| set.len()))
        .ok()
        .flatten()
}

/// Is an evidence window open on this thread? `None` when the flag could not be read.
///
/// Exists so `aterm_spec::xref::StepEvidence::step` can check its window SURVIVED the closure it ran.
/// The exit disarms unconditionally, so a nested `step` — or any hand call to
/// [`disarm_entered_anchors`] — inside the drive closure closes the outer window early,
/// and the outer action is then credited with whatever the inner window recorded.
// Skip: thread-local read (`LocalKey::try_with`, `Cell::get`). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn window_is_armed() -> Option<bool> {
    ARMED.try_with(core::cell::Cell::get).ok()
}

/// Close this thread's window and hand over what it recorded, leaving the record empty:
/// the half of a window split across threads that a WORKER runs (it opened its own
/// window with [`reset_entered_anchors`]; the caller it works for takes the result into
/// its own with [`merge_entered_anchors`]).
///
/// `None` when the record could not be taken (a destroyed TLS slot or an outstanding
/// borrow): a failure to observe, kept apart from an empty window for the reason
/// [`entered_anchor_count`] gives. `None` too, with the record left alone, when no
/// window is open on this thread: a closed window's record is not this work's — a
/// `aterm_spec::xref::StepEvidence::step` disarms on exit and leaves its record readable, so a worker
/// on a reused thread that never opened a window of its own, or one a nested step
/// closed, would otherwise hand over a stale or partial record as its own
/// ([`merge_entered_anchors`] refuses a closed window for the same reason).
// Skip: thread-local bookkeeping (`LocalKey::try_with`, `RefCell::try_borrow_mut`,
// `mem::take`). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn take_entered_anchors() -> Option<BTreeSet<&'static str>> {
    if window_is_armed() != Some(true) {
        return None;
    }
    disarm_entered_anchors();
    ENTERED_ANCHORS
        .try_with(|cell| {
            cell.try_borrow_mut()
                .ok()
                .map(|mut set| core::mem::take(&mut *set))
        })
        .ok()
        .flatten()
}

/// Credit this thread's OPEN window with `ids`: the record a thread doing part of this
/// window's work took with [`take_entered_anchors`]. The per-thread isolation stands:
/// nothing crosses threads unless the window's owner merges it, so two windows on two
/// threads still never share an entry.
///
/// Returns whether `ids` were recorded. `false`, with nothing recorded, when no window
/// is open on this thread (the probe records nothing outside one either, and a window
/// a nested step closed must not be credited) or its record cannot be written; a
/// caller asserts on it, as on [`reset_entered_anchors`].
// Skip: thread-local bookkeeping (`LocalKey::try_with`, `RefCell::try_borrow_mut`,
// `BTreeSet::extend`). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn merge_entered_anchors(ids: BTreeSet<&'static str>) -> bool {
    window_is_armed() == Some(true)
        && ENTERED_ANCHORS
            .try_with(|cell| cell.try_borrow_mut().map(|mut set| set.extend(ids)).is_ok())
            .unwrap_or(false)
}
