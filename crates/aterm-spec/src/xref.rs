// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The compiler-collectable source↔spec cross-reference (TRUST_NATIVE_TLA, §2).
//!
//! `#[refines]`, `#[spec_unmodeled]` and `#[spec_invariant]` each emit a
//! [`registry::submit!`](crate::registry::submit) of a record, so the full set of source↔spec
//! bindings is collectable without scanning source text.
//!
//! * **Source → spec**: [`RefinementAnchor`], [`Waiver`] and [`InvariantAnchor`] records,
//!   collected by [`refinements`], [`waivers`] and [`invariant_anchors`].
//! * **Spec → source**: the embedded [`Model`](crate::derive::Model) registry
//!   ([`model_registry`]) and [`Model::anchors`](crate::derive::Model::anchors), plus external
//!   `.tla` parsed by [`TlaSpec`](crate::tla_check::TlaSpec).
//!
//! [`check_closure`] enforces the four bidirectional obligations of §2.2 over both kinds of
//! `SpecModule`, with coverage scoped to actively bound machines.
//!
//! The registry sees only `submit!`s from object code linked into the running binary.
//! Anchor-bearing dependencies enable their `spec-anchors` features for the `aterm-gui` unit-test
//! build, whose closure links every participating crate, so the full `spec_xref_closure` gate lives
//! in `aterm-gui`; a gate in one library would see only its own anchors.

use std::collections::{BTreeMap, BTreeSet};

use crate::derive::Model;
use crate::tla_check::TlaSpec;

/// A source→spec refinement binding, emitted by `#[refines(machine, action, …)]`. Its fields are
/// `&'static str` so it lives in a `static` submitted at load time.
#[derive(Debug, Clone, Copy)]
pub struct RefinementAnchor {
    /// The TLA+ machine name (e.g. `"terminal_modes"`) — matches a `Model::name`
    /// (lower/CamelCase resolved by [`machine_matches`]) or an external module name.
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
    /// The execution-evidence key: the id the `#[refines]` macro also injects as a fn-entry probe
    /// into the annotated body, so [`entered_anchor_ids`] can say whether the function ran. Shape
    /// `"<machine>::<action> @ <rust_method>"`, from the same expansion as the probe.
    ///
    /// `""` means not instrumented (a `const fn` body, or an item that is not a fn with a block).
    /// That is not a pass: a gate that demands execution evidence treats it as a failure
    /// ([`StepAudit::uninstrumented`]), or moving an anchor onto a `const fn` would opt it out.
    pub entry_id: &'static str,
}

/// An explicit "this fn is intentionally not modeled" waiver, emitted by
/// `#[spec_unmodeled(reason, …)]`. `machine` and `action` are optional (the bare `reason="…"`
/// bypass-setter form leaves them `""`); when present, the waiver discharges that action for the
/// coverage obligation.
#[derive(Debug, Clone, Copy)]
pub struct Waiver {
    pub machine: &'static str,
    pub action: &'static str,
    pub reason: &'static str,
    pub rust_method: &'static str,
    pub location: &'static str,
}

/// A source→spec invariant binding, emitted by `#[spec_invariant(id, machine, …)]`. `machine` is
/// optional (`""` when absent); when present it must resolve to a registered `SpecModule`
/// (obligation 4).
#[derive(Debug, Clone, Copy)]
pub struct InvariantAnchor {
    pub machine: &'static str,
    pub id: &'static str,
    pub rust_method: &'static str,
    pub location: &'static str,
}

crate::registry::collect!(RefinementAnchor);
crate::registry::collect!(Waiver);
crate::registry::collect!(InvariantAnchor);

/// All [`RefinementAnchor`]s linked into the current binary.
pub fn refinements() -> impl Iterator<Item = &'static RefinementAnchor> {
    crate::registry::iter::<RefinementAnchor>()
}

/// All [`Waiver`]s linked into the current binary.
pub fn waivers() -> impl Iterator<Item = &'static Waiver> {
    crate::registry::iter::<Waiver>()
}

/// All [`InvariantAnchor`]s linked into the current binary.
pub fn invariant_anchors() -> impl Iterator<Item = &'static InvariantAnchor> {
    crate::registry::iter::<InvariantAnchor>()
}

// ---------------------------------------------------------------------------
// Execution evidence: the runnable fragment of obligation 2.
// ---------------------------------------------------------------------------
//
// The string closure checks only that an anchor names a real action and that every action of an
// active machine is named; nothing ties the anchor to a fn that does the thing. So in
// `cfg(any(test, feature = "spec-anchors"))` builds (the cfg the attribute is under) the macro also
// injects, as the first statement of the annotated body, a call to [`note_entered`] with the
// anchor's [`RefinementAnchor::entry_id`].
//
// The cfg gates the probe call sites, not this module: the thread-locals and fns below are in
// every build of `aterm-spec`, since the cfg that strips a probe is the annotated crate's. With no
// call site, no shipping path pays even the `bool` load.
//
// Per step, not per run: a window around a whole conformance would pass an anchor moved onto any
// fn the run touches. [`StepEvidence::step`] clears the record, runs the one shipping call that
// drives an action, and snapshots what was entered. A step that drives a `Terminal::process` batch
// still enters several anchored fns at once, so [`StepAudit::stray`] also checks the converse (a
// fn that is the seam for `{B, C}` is not entered by a window driving `D`), and
// [`StepAudit::interchangeable`] prints what entry alone leaves ambiguous.
//
// Thread-local, so a window sees only its own thread's calls; a process-wide set would be satisfied
// by any parallel test. Work a window spreads over other threads is credited by hand: each worker
// opens a window, [`take_entered_anchors`] hands its record back, and [`merge_entered_anchors`]
// credits the owner's window.
//
// Armed only inside a step window: the probe sits in every annotated body, some of them
// per-character hot paths (`Terminal::write_char`), and outside a window it is one thread-local
// `bool` load and a branch.
//
// This is function-entry evidence, not branch evidence: a fn carrying several anchors marks them
// all when entered. `Terminal::post_process` (4 `SelectionCustody` actions) and
// `Terminal::note_output_custody` (7 `PressCustody` actions) run on every VT batch, so permuting
// either's labels changes nothing observable. [`StepAudit::summary`] derives and prints that
// count; [`StepAudit::shared_site`] lists such sites from the anchor table and
// [`StepAudit::indiscriminate`] from the run. An anchor moved onto a fn whose entry pattern is
// exactly the claimed action set (or into an unanchored fn, which has no probe) still passes; full
// symbol and branch resolution is `trust-ir`'s job.

thread_local! {
    /// Whether a [`StepEvidence::step`] window is open on this thread. The probe reads this first,
    /// so an unarmed probe costs one TLS `bool` load; `const`-initialized, so no lazy-init check.
    static ARMED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };

    /// Anchor ids whose annotated fn was entered on this thread inside the open
    /// window. `BTreeSet` (not `HashSet`) so the initializer is `const` — the TLS
    /// costs nothing until the first probe fires.
    static ENTERED_ANCHORS: core::cell::RefCell<BTreeSet<&'static str>> =
        const { core::cell::RefCell::new(BTreeSet::new()) };
}

/// Record that the fn carrying `entry_id` was entered on this thread. Called only by the probe
/// `#[refines]` injects into every annotated body, so it is:
///
/// * nearly free when no gate is watching: the `ARMED` read returns before any borrow or insert;
/// * panic-free in any annotated fn, even during TLS teardown or re-entrantly:
///   [`std::thread::LocalKey::try_with`] tolerates a destroyed slot and `try_borrow_mut` a
///   re-entrant call, and both drop the record rather than unwind.
// Skip: thread-local bookkeeping over std bodies the prover lacks. Verification tooling.
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

/// Open an evidence window: forget this thread's recorded entries and arm the probe. Returns
/// whether the window is open (record empty, probe armed); a failure would leave the previous
/// conformance's entries in place, so a caller fails loudly on `false` ([`StepEvidence::step`]
/// asserts it).
// Skip: thread-local bookkeeping over std bodies the prover lacks. Verification tooling.
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

/// The anchor ids entered on this thread since the last [`reset_entered_anchors`]. Lossy: a
/// destroyed TLS slot or an outstanding borrow reads as an empty set, so assert emptiness with
/// [`entered_anchor_count`] instead.
// Skip: thread-local read over std bodies the prover lacks. Verification tooling.
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

/// How many ids are recorded on this thread; `None` when the record cannot be read (a destroyed
/// TLS slot or an outstanding borrow). `Some(0)` is a positive observation of an empty record, so
/// [`StepEvidence::step`]'s post-reset check is independent of [`reset_entered_anchors`]'s return.
// Skip: thread-local read over std bodies the prover lacks. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn entered_anchor_count() -> Option<usize> {
    ENTERED_ANCHORS
        .try_with(|cell| cell.try_borrow().ok().map(|set| set.len()))
        .ok()
        .flatten()
}

/// Whether an evidence window is open on this thread; `None` when the flag cannot be read. Lets
/// [`StepEvidence::step`] check that its window survived the closure: a nested `step`, or a hand
/// call to [`disarm_entered_anchors`], closes the outer window early.
// Skip: thread-local read (`LocalKey::try_with`, `Cell::get`). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn window_is_armed() -> Option<bool> {
    ARMED.try_with(core::cell::Cell::get).ok()
}

/// Close this thread's window and hand over its record, leaving it empty: the worker's half of a
/// window split across threads (it opened its own with [`reset_entered_anchors`]; the owner takes
/// the result with [`merge_entered_anchors`]).
///
/// `None` when the record cannot be taken, or, with the record left alone, when no window is open
/// on this thread: a closed window's record (a finished step's, or one a nested step closed) is
/// not this work's.
// Skip: thread-local bookkeeping over std bodies the prover lacks. Verification tooling.
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

/// Credit this thread's open window with `ids`, a record a worker took with
/// [`take_entered_anchors`]; nothing crosses threads unless the owner merges it. Returns whether
/// `ids` were recorded: `false` when no window is open here or its record cannot be written, and
/// the caller asserts on it.
// Skip: thread-local bookkeeping over std bodies the prover lacks. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn merge_entered_anchors(ids: BTreeSet<&'static str>) -> bool {
    window_is_armed() == Some(true)
        && ENTERED_ANCHORS
            .try_with(|cell| cell.try_borrow_mut().map(|mut set| set.extend(ids)).is_ok())
            .unwrap_or(false)
}

/// What one conformance step entered: the action it claims to drive, and the anchor ids whose
/// annotated fn ran inside that step's window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    /// The machine action this step drives, spelled as the anchors spell it.
    pub action: &'static str,
    /// Anchor ids entered between the window opening and closing.
    pub entered: BTreeSet<&'static str>,
}

/// A machine's per-step execution evidence, accumulated by its Tier-1 conformance (the only party
/// that knows where one step ends and the next begins) and audited by the gate that ran it.
#[derive(Debug, Clone)]
pub struct StepEvidence {
    machine: &'static str,
    steps: Vec<StepRecord>,
}

impl StepEvidence {
    /// A fresh, empty ledger for `machine` (spelled as [`machine_matches`] accepts).
    #[must_use]
    pub fn new(machine: &'static str) -> Self {
        Self {
            machine,
            steps: Vec::new(),
        }
    }

    /// Drive one step of the conformance inside its own evidence window.
    ///
    /// `drive` must be the shipping call for `action` and nothing else (no pre-state read,
    /// projection or read-back): everything inside the window is evidence for `action`, so a
    /// harness accessor inside it would let an anchor parked there discharge a shipping
    /// obligation. Windows must not nest; the arm bit is re-read after `drive` returns.
    ///
    /// # Panics
    ///
    /// If the window cannot be opened ([`reset_entered_anchors`]), the record is not observably
    /// empty once it is ([`entered_anchor_count`]), or the window was closed while `drive` ran
    /// ([`window_is_armed`]): each would let the audit pass on stale evidence.
    // Skip: thread-local window bookkeeping around a caller-supplied closure. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn step<T>(&mut self, action: &'static str, drive: impl FnOnce() -> T) -> T {
        assert!(
            reset_entered_anchors(),
            "{}::{action}: could not open an execution-evidence window (the thread-local \
             record refused to clear or the probe refused to arm). Auditing anyway would \
             pass on the PREVIOUS step's entries, which is a total fail-open.",
            self.machine
        );
        assert_eq!(
            entered_anchor_count(),
            Some(0),
            "{}::{action}: the anchor-entry record is not OBSERVABLY empty immediately \
             after a reset that reported success — the audit below would pass on stale \
             entries. `Some(n>0)` means the clear did not take; `None` means the record \
             could not be read at all, which is the state the old \
             `entered_anchor_ids().is_empty()` form silently PASSED on.",
            self.machine
        );
        let out = drive();
        assert_eq!(
            window_is_armed(),
            Some(true),
            "{}::{action}: the evidence window was CLOSED while this step ran — a nested \
             `StepEvidence::step`, or a hand call to `disarm_entered_anchors`, inside the \
             drive closure. Windows must not nest: the exit disarms unconditionally, so \
             the entries this step is about to be credited with are the INNER window's, \
             not its own.",
            self.machine
        );
        let entered = entered_anchor_ids();
        disarm_entered_anchors();
        self.steps.push(StepRecord { action, entered });
        out
    }

    /// The machine this ledger is about.
    #[must_use]
    pub fn machine(&self) -> &'static str {
        self.machine
    }

    /// Cross the ledger with the machine's linked anchors.
    // Skip: set algebra over the registry walk. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn audit(&self) -> StepAudit {
        audit_steps(self.machine, &self.steps)
    }
}

/// The execution-evidence verdict for one machine, crossing its linked anchors with a
/// [`StepEvidence`] ledger. Every field is derived from the two, including the limits
/// ([`Self::shared_site`], [`Self::interchangeable`], [`Self::entered_under`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAudit {
    /// The machine, spelled as the caller spelled it.
    pub machine: String,
    /// How many of the machine's anchors are linked into this binary (probed or not).
    pub anchors: usize,
    /// The distinct actions those anchors cover. The gate asserts this set, not a count: an action
    /// may have several anchor sites.
    pub actions: BTreeSet<&'static str>,
    /// How many step windows the conformance recorded.
    pub steps: usize,
    /// Anchors the macro could not probe (`entry_id == ""`), by `rust_method`: a failure for a
    /// machine claiming execution evidence.
    pub uninstrumented: Vec<&'static str>,
    /// `entry_id`s claimed by more than one anchor, which would let one fn's entry
    /// discharge another's obligation.
    pub ambiguous: Vec<&'static str>,
    /// Anchored actions with no step in the ledger: the conformance never claimed to drive them,
    /// so nothing was checked about their anchors.
    pub undriven: Vec<&'static str>,
    /// Anchored actions that were driven and whose annotated function was entered in no step of
    /// their own: the failure list (an anchor on a stub, on a fn the action's step never reaches,
    /// or on an unrelated fn).
    pub unwitnessed: Vec<&'static str>,
    /// One entry per window that did not enter its own action's function, named by that action
    /// (so an action driven three times can appear twice). [`Self::unwitnessed`] is an OR across
    /// an action's windows; this is the AND.
    pub blind_steps: Vec<&'static str>,
    /// Step tags that match no anchored action of this machine — a typo in the
    /// conformance, checking nothing.
    pub unanchored_steps: Vec<&'static str>,
    /// Annotated fn → the actions of this machine anchored on it.
    pub by_function: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Actions sharing their annotated fn with a sibling action, which entry alone cannot tell
    /// apart. Read off the anchor table; [`Self::indiscriminate`] is the run-derived twin.
    pub shared_site: BTreeSet<&'static str>,
    /// Action → the anchored functions every window driving it entered: what a window pins.
    /// `witnessed` needs the action's own function in this set, and an anchor moved onto any other
    /// member would be witnessed the same, so `1` pins the action to its function and `k` cannot
    /// tell it from `k - 1` others.
    ///
    /// A lower bound, to be said wherever it is printed: only an anchored function runs a probe,
    /// so an unanchored one the same window calls is invisible here and would also pass.
    pub interchangeable: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Anchored function of this machine → the actions whose windows entered it. A function with
    /// more than one action is entered under all of them, the vacuous-discharge fact (true of
    /// `Terminal::post_process` and `Terminal::note_output_custody`, entered on every batch).
    pub entered_under: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Actions one of whose anchored functions was entered by another action's window, so entry
    /// says the function ran, not which action fired: the run-derived twin of
    /// [`Self::shared_site`]. With [`Self::stray`] and [`Self::blind_steps`] empty the two are
    /// equal.
    pub indiscriminate: BTreeSet<&'static str>,
    /// Anchored function of this machine → the actions whose windows entered it that it is not
    /// anchored for.
    ///
    /// The within-window detector: [`Self::unwitnessed`] asks whether a function ran when its own
    /// action fired; this asks whether it ran when something else fired. A function that is the
    /// seam for `{B, C}` has no business being entered by a window driving `D`, so an anchor moved
    /// onto another function in the same window is caught exactly when that function's entry
    /// pattern is wider than the actions it claims (`SelectionCustody::UniformScroll` parked on
    /// `Terminal::note_output_custody` lands here, since that function also runs in the damage and
    /// invalidation windows).
    ///
    /// It can fire on a structurally shared function (entered on every batch, in a machine that
    /// drives a batch-shaped action it is not anchored for), whose entry is then no evidence for
    /// its own actions. Anchor it for that action too, narrow the window, or move the anchor to a
    /// specific seam.
    pub stray: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Per step window, in the order driven: the action it drives and the distinct anchored
    /// functions it entered. A window with one pins its action to it; a window with `k` (a whole
    /// `Terminal::process` batch, say) tells none of them apart. The same lower bound as
    /// [`Self::interchangeable`].
    pub window_reach: Vec<(&'static str, BTreeSet<&'static str>)>,
    /// Anchor sites of a multi-site action that no window of that action ever entered.
    /// `witnessed` is an OR across sites, so a second site is never required to run; single-site
    /// actions are excluded, since for them this is [`Self::unwitnessed`].
    pub dark_sites: Vec<&'static str>,
    /// How many anchor sites belong to an action with more than one: the denominator of
    /// [`Self::dark_sites`], so `0` dark sites over `0` reads as "no action has a second site
    /// yet", not an all-clear.
    pub multi_site_anchors: usize,
    /// Steps whose window entered an anchor of their own action.
    pub witnessed_steps: usize,
}

impl StepAudit {
    /// Did every anchored action get driven and witnessed, with none unprobed,
    /// ambiguous, or tagged by a step that names no action?
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.uninstrumented.is_empty()
            && self.ambiguous.is_empty()
            && self.undriven.is_empty()
            && self.unwitnessed.is_empty()
            && self.blind_steps.is_empty()
            && self.stray.is_empty()
            && self.unanchored_steps.is_empty()
    }

    /// The widest interchangeability set: `(action, k)` for the action whose windows
    /// pin its anchor least — `k` already-anchored functions any of which would pass.
    #[must_use]
    pub fn widest_window(&self) -> (&'static str, usize) {
        self.interchangeable
            .iter()
            .map(|(a, f)| (*a, f.len()))
            .max_by_key(|(a, n)| (*n, core::cmp::Reverse(*a)))
            .unwrap_or(("<no window>", 0))
    }

    /// How many windows entered more than one anchored function — the windows inside
    /// which this check distinguishes nothing — and the widest of them, spelled out:
    /// the action it drives and every anchored function it entered. Derived, so no
    /// caller has to name those functions in prose and be wrong about it later.
    #[must_use]
    pub fn wide_windows(&self) -> (usize, &'static str, Vec<&'static str>) {
        let wide = self
            .window_reach
            .iter()
            .filter(|(_, f)| f.len() > 1)
            .count();
        let widest = self
            .window_reach
            .iter()
            .max_by_key(|(a, f)| (f.len(), core::cmp::Reverse(*a)));
        match widest {
            None => (wide, "<no window>", Vec::new()),
            Some((action, fns)) => (wide, action, fns.iter().copied().collect()),
        }
    }

    /// The evidence's arithmetic: what it pins and what it does not, in numbers.
    ///
    /// Every number is a fact about the run, not a quality score. `shared_site` and the
    /// `indiscriminate` count go down when an anchor is moved somewhere wrong, since a wrong
    /// function carries fewer of the machine's actions: an action taken off a four-action seam and
    /// parked on a function only its own window enters passes [`Self::stray`], and the shared
    /// figure falls.
    // Skip: string formatting over the derived fields. Verification tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn summary(&self) -> String {
        let shared: Vec<&str> = self.shared_site.iter().copied().collect();
        let (widest_action, widest) = self.widest_window();
        let (wide, widest_win_action, widest_win_fns) = self.wide_windows();
        let widest_reach = widest_win_fns.len();
        let pinned_exactly = self
            .interchangeable
            .values()
            .filter(|f| f.len() <= 1)
            .count();
        let co: Vec<String> = self
            .entered_under
            .iter()
            .filter(|(_, acts)| acts.len() > 1)
            .map(|(f, acts)| format!("{f}<-{}", acts.len()))
            .collect();
        let indiscriminate: Vec<&str> = self.indiscriminate.iter().copied().collect();
        format!(
            "{} action(s) on {} anchor site(s) over {} distinct function(s), driven by \
             {} step window(s), {}/{} of which entered their own action's function; and no \
             function anchored FOR THIS MACHINE was entered by a window driving an \
             action it is not anchored for (scope matters: a function carrying anchors \
             for a DIFFERENT machine runs its own probe and is invisible to this \
             audit). HOW WIDE A WINDOW IS: {} of the {} windows entered more than \
             one already-anchored function — the widest is `{widest_win_action}`'s, which \
             entered {}: {widest_win_fns:?}. WHAT A WINDOW ALONE WOULD PIN: entry in its \
             own window pins an action's anchor only to the set of already-anchored \
             functions that EVERY window of that action entered — {} of the {} action(s) \
             driven are pinned to exactly one that way, the widest being `{widest_action}` \
             at {widest}, and those are LOWER BOUNDS, since only a function that already \
             carries an anchor runs a probe and an unanchored function inside the same \
             window is invisible to the count. What narrows that set from the other side \
             is the ENTRY-PATTERN check: a candidate inside the window is rejected unless \
             the windows that enter it are exactly the windows of the actions it claims. \
             WHAT NEITHER CAN TELL APART: {} of {} action(s) sit on a function \
             carrying more than one action of this machine ({shared:?}), so one entry \
             marks every anchor on it, and permuting those labels among themselves is \
             invisible to everything here; and — derived from the RUN rather than from the \
             anchor table — {} of {} action(s) were discharged by a function that some \
             OTHER action's window also entered ({co:?}, fn<-actions), i.e. \
             {indiscriminate:?}. The two are read from different places — one off the \
             anchor table, the other off the run — but NOTHING HERE COMPARES THEM, and \
             on a ledger where every window enters every function anchored on it they \
             cannot disagree. Do not read their overlap as a check; it is two views of \
             the same shape, printed so a reader can see it from both sides. SITES: `witnessed` is an OR \
             across an action's anchor sites, so a second site is never required to run \
             at all — {} of the {} site(s) that \
             belong to a multi-site action were never entered by any window of their own \
             action (a zero numerator over a zero denominator means no action has a \
             second site yet, not that the OR is safe). NONE OF THE ABOVE IS A QUALITY \
             SCORE: they describe the shape of the evidence, and two of them — the \
             shared-function count and the indiscriminate count — go DOWN when an anchor \
             is moved somewhere wrong, because the wrong function carries fewer actions.",
            self.actions.len(),
            self.anchors,
            self.by_function.len(),
            self.steps,
            self.witnessed_steps,
            self.steps,
            wide,
            self.steps,
            widest_reach,
            pinned_exactly,
            self.interchangeable.len(),
            self.shared_site.len(),
            self.actions.len(),
            self.indiscriminate.len(),
            self.actions.len(),
            self.dark_sites.len(),
            self.multi_site_anchors,
        )
    }
}

/// The function half of an entry id (`"<machine>::<action> @ <fn>"`). A window observes functions,
/// not anchors (two anchors on one fn always fire together), so [`StepAudit::interchangeable`] and
/// [`StepAudit::entered_under`] are keyed by this half.
fn fn_of(entry_id: &'static str) -> &'static str {
    entry_id.rsplit_once(" @ ").map_or(entry_id, |(_, f)| f)
}

/// Cross `machine`'s linked anchors with a per-step ledger. `machine` is resolved with
/// [`machine_matches`], so the caller may spell it the way the anchors do.
// Skip: registry walk feeding the pure set algebra below. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn audit_steps(machine: &str, steps: &[StepRecord]) -> StepAudit {
    let linked: Vec<&'static RefinementAnchor> = refinements()
        .filter(|r| machine_matches(r.machine, machine))
        .collect();
    audit_steps_of(machine, &linked, steps)
}

/// The pure half of [`audit_steps`]: set algebra over an explicit anchor list, so the
/// derivations can be exercised on synthetic ledgers rather than only on whatever the
/// test binary happened to link.
// Skip: set algebra over std bodies the prover lacks. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_lines)] // one derivation per `StepAudit` field, in field order
fn audit_steps_of(
    machine: &str,
    linked: &[&'static RefinementAnchor],
    steps: &[StepRecord],
) -> StepAudit {
    let mut anchors = 0usize;
    let mut actions: BTreeSet<&'static str> = BTreeSet::new();
    let mut ids_of_action: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    let mut by_function: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    let mut uninstrumented: Vec<&'static str> = Vec::new();
    let mut seen: BTreeMap<&'static str, usize> = BTreeMap::new();
    for r in linked.iter().copied() {
        anchors += 1;
        actions.insert(r.action);
        by_function
            .entry(r.rust_method)
            .or_default()
            .insert(r.action);
        if r.entry_id.is_empty() {
            uninstrumented.push(r.rust_method);
            continue;
        }
        *seen.entry(r.entry_id).or_insert(0) += 1;
        ids_of_action
            .entry(r.action)
            .or_default()
            .insert(r.entry_id);
    }
    uninstrumented.sort_unstable();
    uninstrumented.dedup();
    let ambiguous: Vec<&'static str> = seen
        .into_iter()
        .filter_map(|(id, n)| if n > 1 { Some(id) } else { None })
        .collect();

    // Per action: was it driven at all, and did any of its own steps enter it?
    let mut undriven: Vec<&'static str> = Vec::new();
    let mut unwitnessed: Vec<&'static str> = Vec::new();
    for action in &actions {
        let ids = ids_of_action.get(action);
        let mut driven = false;
        let mut witnessed = false;
        for s in steps.iter().filter(|s| s.action == *action) {
            driven = true;
            if ids.is_some_and(|ids| ids.iter().any(|id| s.entered.contains(id))) {
                witnessed = true;
            }
        }
        if !driven {
            undriven.push(action);
        } else if !witnessed {
            unwitnessed.push(action);
        }
    }

    let unanchored_steps: Vec<&'static str> = {
        let mut v: Vec<&'static str> = steps
            .iter()
            .map(|s| s.action)
            .filter(|a| !actions.contains(a))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };

    let witnessed_of = |s: &StepRecord| {
        ids_of_action
            .get(s.action)
            .is_some_and(|ids| ids.iter().any(|id| s.entered.contains(id)))
    };
    let witnessed_steps = steps.iter().filter(|s| witnessed_of(s)).count();

    // The AND to `unwitnessed`'s OR. A mistyped tag is in `unanchored_steps`, not a blind
    // window.
    let blind_steps: Vec<&'static str> = steps
        .iter()
        .filter(|s| actions.contains(&s.action) && !witnessed_of(s))
        .map(|s| s.action)
        .collect();

    // What each window entered, as functions.
    let fns_of_step: Vec<BTreeSet<&'static str>> = steps
        .iter()
        .map(|s| s.entered.iter().copied().map(fn_of).collect())
        .collect();

    // Per action, the functions every one of its windows entered: any member would have passed
    // as the action's own.
    let mut interchangeable: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    for action in &actions {
        let mut windows = steps
            .iter()
            .zip(&fns_of_step)
            .filter(|(s, _)| s.action == *action)
            .map(|(_, f)| f);
        if let Some(first) = windows.next() {
            let set = windows.fold(first.clone(), |acc, f| {
                acc.intersection(f).copied().collect()
            });
            interchangeable.insert(action, set);
        }
    }

    // Per anchored function of this machine, the actions whose windows entered it.
    let mut entered_under: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    for (s, fns) in steps.iter().zip(&fns_of_step) {
        if !actions.contains(&s.action) {
            continue;
        }
        for f in fns.iter().copied().filter(|f| by_function.contains_key(f)) {
            entered_under.entry(f).or_default().insert(s.action);
        }
    }

    // An action is indiscriminate when a function it is anchored on was entered by another
    // action's window: that site alone could have discharged it.
    let indiscriminate: BTreeSet<&'static str> = actions
        .iter()
        .copied()
        .filter(|action| {
            by_function.iter().any(|(f, acts)| {
                acts.contains(action)
                    && entered_under
                        .get(f)
                        .is_some_and(|under| under.iter().any(|a| a != action))
            })
        })
        .collect();

    let window_reach: Vec<(&'static str, BTreeSet<&'static str>)> = steps
        .iter()
        .zip(&fns_of_step)
        .map(|(s, f)| (s.action, f.clone()))
        .collect();

    // Entered under an action this function is not anchored for (see `StepAudit::stray`).
    let stray: BTreeMap<&'static str, BTreeSet<&'static str>> = entered_under
        .iter()
        .filter_map(|(f, under)| {
            let own = by_function.get(f)?;
            let extra: BTreeSet<&'static str> = under.difference(own).copied().collect();
            (!extra.is_empty()).then_some((*f, extra))
        })
        .collect();

    // Sites of a multi-site action no window of that action entered, and the eligible count.
    let multi_site_anchors: usize = ids_of_action
        .values()
        .map(BTreeSet::len)
        .filter(|n| *n > 1)
        .sum();
    let dark_sites: Vec<&'static str> = ids_of_action
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .flat_map(|(action, ids)| {
            ids.iter().copied().filter(move |id| {
                !steps
                    .iter()
                    .any(|s| s.action == *action && s.entered.contains(id))
            })
        })
        .collect();

    let shared_site: BTreeSet<&'static str> = by_function
        .values()
        .filter(|a| a.len() > 1)
        .flat_map(|a| a.iter().copied())
        .collect();

    StepAudit {
        machine: machine.to_string(),
        anchors,
        actions,
        steps: steps.len(),
        uninstrumented,
        ambiguous,
        undriven,
        unwitnessed,
        blind_steps,
        unanchored_steps,
        by_function,
        shared_site,
        interchangeable,
        entered_under,
        indiscriminate,
        stray,
        window_reach,
        dark_sites,
        multi_site_anchors,
        witnessed_steps,
    }
}

/// One registered model. `ty_model!` submits every model it writes and `#[model]` a hand-built
/// one, so the registry is exactly the set of models linked into the binary.
#[derive(Debug, Clone, Copy)]
pub struct RegisteredModel {
    /// Builds the model.
    pub build: fn() -> Model,
    /// The configurations it declares beside the committed one (`scenario` in `ty_model!`): each
    /// a set of constant overrides the real code also runs under, swept like the committed one.
    pub scenarios: &'static [&'static [(&'static str, i64)]],
    /// Where it was written (`file:line`).
    pub location: &'static str,
}

/// One registered liveness obligation, submitted by `#[liveness(model)]` with the model it is
/// stated over.
#[derive(Debug, Clone, Copy)]
pub struct RegisteredLiveness {
    /// Builds the model the obligation holds of.
    pub model: fn() -> Model,
    /// Builds the obligation.
    pub liveness: fn() -> crate::derive::Liveness,
    /// Where it was written (`file:line`).
    pub location: &'static str,
}

crate::registry::collect!(RegisteredModel);
crate::registry::collect!(RegisteredLiveness);

/// Every model linked into this binary, in name order. The non-vacuity sweep holds each design
/// invariant to a catching mutant, `spec-check` re-checks each model under `ty`, and the closure
/// resolves an anchor's `machine` against this set.
// Skip: calls every registered model constructor. Spec tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn model_registry() -> Vec<Model> {
    registered().into_values().map(|(m, _, _)| m).collect()
}

/// Every registered model in every configuration it declares (the committed one first, then each
/// `scenario`), in [`model_registry`]'s order. The sweeps hold all of them: a law, an action or a
/// mutant may matter only in a configuration the committed one does not reach.
// Skip: same class as `model_registry`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn model_configurations() -> Vec<Vec<Model>> {
    registered()
        .into_values()
        .map(|(m, scenarios, _)| {
            let scenarios = scenarios
                .iter()
                .map(|overrides| crate::interp::with_consts(&m, overrides));
            std::iter::once(m.clone()).chain(scenarios).collect()
        })
        .collect()
}

/// Every liveness obligation linked into this binary, beside the registered model it is stated
/// over, in (model, obligation) name order.
// Skip: same class as `model_registry`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn liveness_registry() -> Vec<(Model, crate::derive::Liveness)> {
    let models = registered();
    let mut entries: Vec<(Model, crate::derive::Liveness)> =
        crate::registry::iter::<RegisteredLiveness>()
            .map(|r| {
                let name = (r.model)().name;
                (models[name].0.clone(), (r.liveness)())
            })
            .collect();
    entries.sort_by(|a, b| a.0.name.cmp(b.0.name).then(a.1.name.cmp(b.1.name)));
    entries
}

/// Every hand-written `.tla` under `aterm-spec-models/specs`, at any depth, sorted: the external
/// modules beside the registered models. One walker, so `spec-check` and the closure hold the
/// same set.
///
/// # Errors
///
/// When the directory cannot be read or holds no spec.
// Skip: a directory walk. Spec tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn hand_written_specs() -> Result<Vec<std::path::PathBuf>, String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "tla") {
                out.push(path);
            }
        }
        Ok(())
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-spec-models/specs");
    let mut specs = Vec::new();
    walk(&dir, &mut specs).map_err(|e| format!("no specs at {}: {e}", dir.display()))?;
    specs.sort();
    if specs.is_empty() {
        return Err(format!("no .tla under {}", dir.display()));
    }
    Ok(specs)
}

/// A registered model, the scenarios it declares, and where it was written.
type Registered = (
    Model,
    &'static [&'static [(&'static str, i64)]],
    &'static str,
);

/// Every registered model by name: each one a `ty_model!` or `#[model]` registers, and each one a
/// `#[liveness]` obligation is stated over, so no obligation's model escapes the sweeps.
///
/// A name is a model's identity (the machine an anchor names, the module `trust-ir` links), so one
/// name is one model: the same model registered twice is one entry.
///
/// # Panics
///
/// When two different models share a name.
// Skip: same class as `model_registry`.
#[cfg_attr(trust_verify, trust::skip)]
fn registered() -> BTreeMap<&'static str, Registered> {
    let mut by_name: BTreeMap<&'static str, Registered> = BTreeMap::new();
    let models =
        crate::registry::iter::<RegisteredModel>().map(|r| ((r.build)(), r.scenarios, r.location));
    let stated_over =
        crate::registry::iter::<RegisteredLiveness>().map(|r| ((r.model)(), &[][..], r.location));
    for (m, scenarios, location) in models.chain(stated_over) {
        match by_name.get_mut(m.name) {
            None => {
                by_name.insert(m.name, (m, scenarios, location));
            }
            Some(known) => {
                assert!(
                    known.0.to_tla() == m.to_tla() && known.0.to_cfg() == m.to_cfg(),
                    "two different models are named `{}` ({} and {}): a name is a model's \
                     identity, so give one of them its own",
                    m.name,
                    known.2,
                    location
                );
                if known.1.is_empty() {
                    known.1 = scenarios;
                }
            }
        }
    }
    by_name
}

/// The action/anchor name set of an embedded [`Model`] — for embedded models the
/// invariant-def and coverage-action namespaces coincide, so both
/// [`SpecModule::invariant_names`] and [`SpecModule::coverage_actions`] use this.
// An insert loop, not `collect`, so Trust L0 sees per-element growth.
// Skip: BTreeSet keyed build + iterator `next` (absent std bodies).
#[cfg_attr(trust_verify, trust::skip)]
fn embedded_action_set(m: &Model) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (_, a) in m.anchors() {
        names.insert(a.to_string());
    }
    names
}

/// A registered spec module — an embedded [`Model`] or a parsed external `.tla`.
/// Obligation 4 (machine resolves) is satisfied by either variant.
pub enum SpecModule {
    /// An embedded `ty_model!`/`derive` model (the default, drift-free).
    Embedded(Model),
    /// A parsed external `.tla` (full-TLA+ design specs; ISOLATION family).
    External(TlaSpec),
}

impl SpecModule {
    /// The machine name this module declares.
    pub fn name(&self) -> &str {
        match self {
            SpecModule::Embedded(m) => m.name,
            SpecModule::External(t) => &t.module_name,
        }
    }

    /// The names a refinement or waiver anchor may resolve to (obligation 1): an embedded model's
    /// actions, or an external `.tla`'s `Next` disjuncts only
    /// ([`coverage_actions`](Self::coverage_actions)), never every top-level definition, so a
    /// `#[refines]` aimed at `Init` or `TypeOK` fails Ob.1 here as in the lowered Trust artifact
    /// (TRUST_VACUITY_GATE §2.4). `#[spec_invariant]` uses the wider
    /// [`invariant_names`](Self::invariant_names).
    pub fn action_names(&self) -> BTreeSet<String> {
        self.coverage_actions()
    }

    /// The names a `#[spec_invariant]` `id` may resolve to: an embedded model's actions, or every
    /// top-level definition of an external `.tla`, since an invariant legitimately names a
    /// non-`Next` def like `TypeOK` (TRUST_VACUITY_GATE §2.4).
    // Skip: BTreeSet keyed build over std bodies the prover lacks. Spec-anchor ledger machinery.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn invariant_names(&self) -> BTreeSet<String> {
        match self {
            SpecModule::Embedded(m) => embedded_action_set(m),
            SpecModule::External(t) => t.actions.clone(),
        }
    }

    /// The action set of the coverage obligation (obligation 3), all of which an actively bound
    /// machine must bind or waive: an embedded model's actions, or an external `.tla`'s `Next`
    /// disjuncts, so coverage never demands a `#[refines]` for `Init`, `Spec`, `TypeOK`, an
    /// invariant or a constant. A spec with no parseable `Next` disjuncts falls back to the full
    /// definition set, so coverage cannot pass vacuously.
    // Skip: BTreeSet keyed build over std bodies the prover lacks. Spec-coverage machinery.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn coverage_actions(&self) -> BTreeSet<String> {
        match self {
            SpecModule::Embedded(m) => embedded_action_set(m),
            SpecModule::External(t) => {
                if t.next_actions.is_empty() {
                    t.actions.clone()
                } else {
                    t.next_actions.clone()
                }
            }
        }
    }
}

/// Whether an anchor's `machine` resolves to a `SpecModule` named `module_name`: anchors are
/// lower_snake (`"terminal_modes"`), models CamelCase (`"TerminalModes"`), so the match is
/// case-insensitive with `_` stripped.
pub fn machine_matches(anchor_machine: &str, module_name: &str) -> bool {
    // Skip: char-filter iterator `next` + String alloc (absent std bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn norm(s: &str) -> String {
        // A push loop, not `collect`, for Trust L0's allocation check.
        let mut out = String::new();
        for c in s.chars() {
            if c == '_' {
                continue;
            }
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
        out
    }
    norm(anchor_machine) == norm(module_name)
}

/// A single obligation failure (for a readable aggregate error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureViolation {
    /// Which §2.2 obligation was violated: 1, 3 or 4. Obligation 2 (the symbol resolves to a live
    /// `DefId`) needs compiler-owned identities this gate does not have; [`StepEvidence`] is its
    /// executable, weaker fragment (per-step function-entry evidence).
    pub obligation: u8,
    pub message: String,
}

/// One module's coverage line for the report. Obligation 3 holds an active machine to full
/// coverage; an inactive one's ratio is only reported.
#[derive(Debug, Clone)]
pub struct MachineCoverage {
    pub machine: String,
    pub total_actions: usize,
    pub bound: BTreeSet<String>,
    pub waived: BTreeSet<String>,
    pub uncovered: BTreeSet<String>,
    pub active: bool,
}

impl MachineCoverage {
    pub fn ratio(&self) -> f64 {
        if self.total_actions == 0 {
            return 1.0;
        }
        // Saturating and `max(1)` only discharge Trust L0's overflow and divisor obligations;
        // neither changes a reachable result.
        let covered = self.bound.len().saturating_add(self.waived.len());
        covered as f64 / self.total_actions.max(1) as f64
    }
}

/// The outcome of [`check_closure`]: the violations (empty == green) and the
/// per-machine coverage ledger (for reporting).
pub struct ClosureReport {
    pub violations: Vec<ClosureViolation>,
    pub coverage: Vec<MachineCoverage>,
}

impl ClosureReport {
    /// Whether the closure holds (no violations).
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }
}

/// One module's three indexed namespaces, keyed by its declared machine name:
/// `(machine name, action set, invariant-def set, coverage-action set)`. They
/// coincide for embedded models and diverge for external `.tla` (see [`check_closure`]).
type ModuleActionIndex = (String, BTreeSet<String>, BTreeSet<String>, BTreeSet<String>);

struct AnchorObligationSpec<'a> {
    site: &'a str,
    machine: &'a str,
    target: Option<&'a str>,
    select: fn(&ModuleActionIndex) -> &BTreeSet<String>,
    authoring_hint: bool,
    noun: &'a str,
    known: &'a str,
}

/// The Ob.4 ("machine exists") and Ob.1 ("action/id exists") arm for one anchor: push a
/// [`ClosureViolation`] when `resolved` is `None`, else when `target` does not resolve in the
/// namespace `select`ed from the module's index. `site` is the anchor's rendered position (e.g.
/// `"#[refines] at src/x.rs:3 (Foo::bar)"`), `authoring_hint` appends the refinement arm's
/// "Either author the model…" tail to the Ob.4 message, `noun`/`known` fix the Ob.1 wording, and
/// a `None` target (a waiver with no action) skips Ob.1.
fn check_anchor_obligations(
    violations: &mut Vec<ClosureViolation>,
    resolved: Option<&ModuleActionIndex>,
    spec: AnchorObligationSpec<'_>,
) {
    let AnchorObligationSpec {
        site,
        machine,
        target,
        select,
        authoring_hint,
        noun,
        known,
    } = spec;
    match resolved {
        None => {
            let tail = if authoring_hint {
                " (embedded Model or external .tla). Either author the model or fix the \
                 machine name."
            } else {
                "."
            };
            violations.push(ClosureViolation {
                obligation: 4,
                message: format!(
                    "{site} names machine `{machine}` which resolves to NO registered \
                     SpecModule{tail}"
                ),
            });
        }
        Some(idx) => {
            let set = select(idx);
            if let Some(t) = target
                && !set.contains(t)
            {
                violations.push(ClosureViolation {
                    obligation: 1,
                    message: format!(
                        "{site} names {noun} `{t}` which does NOT exist in machine \
                         `{machine}`. Known {known}: {set:?}"
                    ),
                });
            }
        }
    }
}

/// Enforce the bidirectional obligations of TRUST_NATIVE_TLA §2.2 over `modules` and the
/// collected refinements, waivers and invariant anchors:
///
/// * **Ob.1, action exists**: every `refines`/`spec_invariant` action (and every waiver action,
///   when present) names a real definition in its module.
/// * **Ob.3, coverage**: every action of an active machine (one with a refinement) is bound or
///   waived, `ratio == 1.0`.
/// * **Ob.4, machine exists**: every `machine` an anchor names resolves to a registered
///   `SpecModule`.
///
/// Coverage is scoped to actively bound machines: many models are bound by Tier-1 conformance
/// rather than per-method `#[refines]`, so the rest are only reported (`active == false`).
///
/// Ob.2 (the symbol resolves to a live `DefId`) is not enforced here. Its runnable fragment,
/// [`StepEvidence`], is asserted by the gate that owns each conformance; this fn stays pure set
/// algebra over the collected records, since callers with no conformance to run and fixtures with
/// hand-built anchors use it, so a green ratio here is a claim about strings.
// Skip: the closure gate's set algebra and formatting. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn check_closure(modules: &[SpecModule]) -> ClosureReport {
    let mut violations = Vec::new();

    // Three namespaces per module (TRUST_VACUITY_GATE §2.4): the action set (refinement and
    // waiver Ob.1), the invariant-def set (`#[spec_invariant]` ids only) and the coverage set
    // (Ob.3). They coincide for embedded models; for an external `.tla` the action and coverage
    // sets are the `Next` disjuncts and the invariant set is every definition.
    let module_actions: Vec<ModuleActionIndex> = modules
        .iter()
        .map(|m| {
            (
                m.name().to_string(),
                m.action_names(),
                m.invariant_names(),
                m.coverage_actions(),
            )
        })
        .collect();

    let resolve = |anchor_machine: &str| -> Option<&ModuleActionIndex> {
        module_actions
            .iter()
            .find(|(name, _, _, _)| machine_matches(anchor_machine, name))
    };

    // ---- Obligation 4 + 1 for refinements ----
    for r in refinements() {
        check_anchor_obligations(
            &mut violations,
            resolve(r.machine),
            AnchorObligationSpec {
                site: &format!("#[refines] at {} ({})", r.location, r.rust_method),
                machine: r.machine,
                target: Some(r.action),
                select: |idx| &idx.1,
                authoring_hint: true,
                noun: "action",
                known: "actions",
            },
        );
    }

    // ---- Obligation 4 + 1 for invariant anchors (machine optional) ----
    for inv in invariant_anchors() {
        if inv.machine.is_empty() {
            continue;
        }
        // An invariant id names a definition, so it resolves against the wider
        // `invariant_names()` (§2.4).
        check_anchor_obligations(
            &mut violations,
            resolve(inv.machine),
            AnchorObligationSpec {
                site: &format!(
                    "#[spec_invariant] at {} ({})",
                    inv.location, inv.rust_method
                ),
                machine: inv.machine,
                target: Some(inv.id),
                select: |idx| &idx.2,
                authoring_hint: false,
                noun: "id",
                known: "definitions",
            },
        );
    }

    // ---- Obligation 4 + 1 for waivers that name a machine/action ----
    for w in waivers() {
        if w.machine.is_empty() {
            // Bare `reason="…"` waiver (bypass-setter form): not tied to a machine,
            // nothing to resolve. It cannot discharge coverage either.
            continue;
        }
        check_anchor_obligations(
            &mut violations,
            resolve(w.machine),
            AnchorObligationSpec {
                site: &format!("#[spec_unmodeled] at {} ({})", w.location, w.rust_method),
                machine: w.machine,
                target: (!w.action.is_empty()).then_some(w.action),
                select: |idx| &idx.1,
                authoring_hint: false,
                noun: "action",
                known: "actions",
            },
        );
    }

    // ---- Obligation 3: coverage over active machines, report over the rest ----
    // Bind/waive sets keyed by the module's declared name (canonical).
    let mut bound: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut waived: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for r in refinements() {
        if let Some((name, _, _, _)) = resolve(r.machine) {
            bound
                .entry(name.clone())
                .or_default()
                .insert(r.action.to_string());
        }
    }
    for w in waivers() {
        if w.machine.is_empty() || w.action.is_empty() {
            continue;
        }
        if let Some((name, _, _, _)) = resolve(w.machine) {
            waived
                .entry(name.clone())
                .or_default()
                .insert(w.action.to_string());
        }
    }

    let mut coverage = Vec::new();
    // Over the coverage set (an external machine's `Next` disjuncts), not every definition.
    for (name, _actions, _inv_defs, cov_actions) in &module_actions {
        let actions = cov_actions;
        let b = bound.get(name).cloned().unwrap_or_default();
        let wv = waived.get(name).cloned().unwrap_or_default();
        let active = !b.is_empty();
        let covered: BTreeSet<String> = b.union(&wv).cloned().collect();
        let uncovered: BTreeSet<String> = actions.difference(&covered).cloned().collect();
        let mc = MachineCoverage {
            machine: name.clone(),
            total_actions: actions.len(),
            bound: b,
            waived: wv,
            uncovered: uncovered.clone(),
            active,
        };
        // Obligation 3 (scoped): an active machine must be fully bound-or-waived.
        if active && !uncovered.is_empty() {
            violations.push(ClosureViolation {
                obligation: 3,
                message: format!(
                    "machine `{}` is actively bound ({} refinement(s)) but {} action(s) are \
                     neither bound nor waived: {:?}. Add a #[refines] or a \
                     #[spec_unmodeled(machine=…, action=…, reason=…)] for each. (ratio = {:.3})",
                    name,
                    mc.bound.len(),
                    uncovered.len(),
                    uncovered,
                    mc.ratio()
                ),
            });
        }
        coverage.push(mc);
    }

    ClosureReport {
        violations,
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tla_check::TlaSpec;

    /// TRUST_VACUITY_GATE §2.4: an external module's `action_names()` is its `Next` disjuncts
    /// only and its `invariant_names()` every top-level definition, so an anchor naming `TypeOK`
    /// fails Ob.1 while an invariant naming it resolves.
    #[test]
    fn external_action_names_is_next_only_invariant_names_is_full_set() {
        let tla = "---- MODULE Fix ----\nVARIABLES x\nInit == x = 0\nApply == x' = 1\n\
                   Next == Apply\nTypeOK == x \\in {0, 1}\n====\n";
        let spec = TlaSpec::parse_str(tla, "Fix.tla").expect("parse");
        let m = SpecModule::External(spec);
        // Next-only action set: Apply yes, TypeOK no.
        assert!(
            m.action_names().contains("Apply"),
            "Apply is a Next disjunct"
        );
        assert!(
            !m.action_names().contains("TypeOK"),
            "TypeOK must NOT be a valid action target (Next-only) — the finding-4 narrowing"
        );
        // Wider invariant-def set: TypeOK resolves as an invariant id.
        assert!(
            m.invariant_names().contains("TypeOK"),
            "TypeOK must resolve as an invariant id (the full def set)"
        );
        assert!(
            m.invariant_names().contains("Apply"),
            "the def set is a superset of actions"
        );
    }

    // ── The per-step audit's derivations, on synthetic ledgers ──────────────
    //
    // The real ledgers come from a gate in another crate; the arithmetic it prints must be
    // checkable without running it.

    /// A synthetic anchor. `entry_id` is spelled exactly as the `#[refines]` macro
    /// spells it, because `fn_of` splits on that shape.
    fn synth(
        action: &'static str,
        method: &'static str,
        entry_id: &'static str,
    ) -> RefinementAnchor {
        RefinementAnchor {
            machine: "Ring",
            action,
            rust_method: method,
            location: "xref.rs:0:0",
            project: "",
            entry_id,
        }
    }

    fn window(action: &'static str, entered: &[&'static str]) -> StepRecord {
        StepRecord {
            action,
            entered: entered.iter().copied().collect(),
        }
    }

    /// The real ledgers' shape in miniature: one function carrying two actions and entered on
    /// every batch (`batch`, the `post_process` stand-in), one private to a third action
    /// (`evict`), and an unrelated anchored function the batch also enters (`chatty`, the
    /// `write_char` stand-in).
    fn mixed_ledger() -> (Vec<RefinementAnchor>, Vec<StepRecord>) {
        let anchors = vec![
            synth("Push", "batch", "Ring::Push @ batch"),
            synth("Pop", "batch", "Ring::Pop @ batch"),
            synth("Evict", "evict", "Ring::Evict @ evict"),
        ];
        let steps = vec![
            window(
                "Push",
                &[
                    "Ring::Push @ batch",
                    "Ring::Pop @ batch",
                    "Other::X @ chatty",
                ],
            ),
            window(
                "Pop",
                &[
                    "Ring::Push @ batch",
                    "Ring::Pop @ batch",
                    "Other::X @ chatty",
                ],
            ),
            window("Evict", &["Ring::Evict @ evict"]),
        ];
        (anchors, steps)
    }

    fn audit_of(anchors: &[RefinementAnchor], steps: &[StepRecord]) -> StepAudit {
        // `audit_steps_of` takes `&'static` anchors because the registry yields them;
        // the leak is a test fixture and bounded by the test.
        let linked: Vec<&'static RefinementAnchor> =
            anchors.iter().map(|a| &*Box::leak(Box::new(*a))).collect();
        audit_steps_of("Ring", &linked, steps)
    }

    #[test]
    fn entered_under_fires_where_the_old_ubiquitous_intersection_could_not() {
        let (anchors, steps) = mixed_ledger();
        let audit = audit_of(&anchors, &steps);
        assert!(
            audit.is_complete(),
            "the fixture ledger must be green: {audit:?}"
        );

        // An intersection across every window is empty by construction on mixed step
        // families: `batch` misses the `Evict` window and `evict` the two batch windows.
        let all: BTreeSet<&str> = steps
            .iter()
            .skip(1)
            .fold(steps[0].entered.clone(), |acc, s| {
                acc.intersection(&s.entered).copied().collect()
            });
        assert!(
            all.is_empty(),
            "the intersection across mixed step families is empty by construction: {all:?}"
        );

        // `entered_under` says the true thing: `batch` is entered under both of its actions, so
        // its entry cannot say which fired.
        assert_eq!(
            audit.entered_under.get("batch").map(BTreeSet::len),
            Some(2),
            "`batch` is entered by the windows of both its actions: {:?}",
            audit.entered_under
        );
        assert_eq!(
            audit.entered_under.get("evict").map(BTreeSet::len),
            Some(1),
            "`evict` is entered by its own action's window only"
        );
        assert!(
            !audit.entered_under.contains_key("chatty"),
            "an anchored fn of ANOTHER machine is not this machine's accounting"
        );
        assert_eq!(
            audit.indiscriminate,
            ["Pop", "Push"].into_iter().collect::<BTreeSet<_>>(),
            "Push and Pop share a function each other's windows enter; Evict does not"
        );
    }

    #[test]
    fn interchangeable_counts_what_a_window_leaves_ambiguous() {
        let (anchors, steps) = mixed_ledger();
        let audit = audit_of(&anchors, &steps);
        // `Push`'s window entered two anchored functions, so `witnessed` alone cannot
        // tell `batch` from `chatty` — that is what this number is for.
        assert_eq!(
            audit.interchangeable.get("Push"),
            Some(&["batch", "chatty"].into_iter().collect::<BTreeSet<_>>()),
            "{:?}",
            audit.interchangeable
        );
        // `Evict`'s window entered exactly one: that window pins its anchor.
        assert_eq!(
            audit.interchangeable.get("Evict").map(BTreeSet::len),
            Some(1)
        );
        assert_eq!(audit.widest_window().1, 2);
        assert!(
            audit.stray.is_empty(),
            "the fixture is honest: {:?}",
            audit.stray
        );
    }

    /// The within-window move, both halves: the one `stray` catches and the one it cannot.
    #[test]
    fn a_move_inside_one_window_is_caught_exactly_when_the_new_site_is_wider() {
        // Caught: `Push` moves onto `chatty`, which the `Push` window does enter (so
        // `witnessed` holds), but the `Pop` window enters it too and it is not anchored for
        // `Pop`: its entry pattern is wider than the action it now claims.
        let moved = vec![
            synth("Push", "chatty", "Ring::Push @ chatty"),
            synth("Pop", "batch", "Ring::Pop @ batch"),
            synth("Evict", "evict", "Ring::Evict @ evict"),
        ];
        let steps = vec![
            window("Push", &["Ring::Push @ chatty", "Ring::Pop @ batch"]),
            window("Pop", &["Ring::Push @ chatty", "Ring::Pop @ batch"]),
            window("Evict", &["Ring::Evict @ evict"]),
        ];
        let caught = audit_of(&moved, &steps);
        assert!(
            caught.unwitnessed.is_empty() && caught.blind_steps.is_empty(),
            "entry-in-its-own-window is satisfied — that check alone would pass this"
        );
        assert_eq!(
            caught.stray.get("chatty"),
            Some(&["Pop"].into_iter().collect::<BTreeSet<_>>()),
            "…and the contrapositive is what fails it: {:?}",
            caught.stray
        );
        assert!(!caught.is_complete());

        // Not caught: `Push` moves onto `private`, which only the `Push` window enters. Its
        // entry pattern is exactly the action it claims, so a per-step entry ledger cannot tell
        // it from the right seam.
        let moved = vec![
            synth("Push", "private", "Ring::Push @ private"),
            synth("Pop", "batch", "Ring::Pop @ batch"),
            synth("Evict", "evict", "Ring::Evict @ evict"),
        ];
        let steps = vec![
            window("Push", &["Ring::Push @ private", "Other::X @ chatty"]),
            window("Pop", &["Ring::Pop @ batch", "Other::X @ chatty"]),
            window("Evict", &["Ring::Evict @ evict"]),
        ];
        let missed = audit_of(&moved, &steps);
        assert!(
            missed.is_complete(),
            "the residual the gate PRINTS: a move onto a function whose entry pattern \
             matches the action set it claims is invisible here — {missed:?}"
        );

        // Nor can anything see two actions that share one function permuted.
        let permuted = vec![
            synth("Pop", "batch", "Ring::Pop @ batch"),
            synth("Push", "batch", "Ring::Push @ batch"),
            synth("Evict", "evict", "Ring::Evict @ evict"),
        ];
        let (base_anchors, base_steps) = mixed_ledger();
        assert_eq!(
            audit_of(&permuted, &base_steps).stray,
            audit_of(&base_anchors, &base_steps).stray,
            "one function carrying two actions cannot tell them apart, however they are \
             labelled — `shared_site` is exactly this and it is printed for it"
        );
    }

    #[test]
    fn a_window_that_misses_its_own_action_is_named_even_when_a_sibling_window_saw_it() {
        let anchors = vec![synth("Evict", "evict", "Ring::Evict @ evict")];
        let steps = vec![
            window("Evict", &["Ring::Evict @ evict"]),
            window("Evict", &["Other::X @ chatty"]),
        ];
        let audit = audit_of(&anchors, &steps);
        assert!(
            audit.unwitnessed.is_empty(),
            "the OR across an action's windows still passes it"
        );
        assert_eq!(
            audit.blind_steps,
            vec!["Evict"],
            "…and the AND names the other one"
        );
        assert_eq!(audit.witnessed_steps, 1);
        assert!(!audit.is_complete(), "which the gate must fail on");
    }

    #[test]
    fn a_second_anchor_site_that_never_runs_is_named_rather_than_carried() {
        let anchors = vec![
            synth("Evict", "evict", "Ring::Evict @ evict"),
            synth("Evict", "batch", "Ring::Evict @ batch"),
        ];
        let steps = vec![window("Evict", &["Ring::Evict @ evict"])];
        let audit = audit_of(&anchors, &steps);
        assert!(
            audit.unwitnessed.is_empty(),
            "`witnessed` ORs across SITES, so one live site discharges the action"
        );
        assert_eq!(
            audit.dark_sites,
            vec!["Ring::Evict @ batch"],
            "…and the site that never ran is reported instead of vanishing"
        );
    }

    /// A worker's window reaches its owner only by take and merge: nothing crosses threads
    /// otherwise, a taken window is closed and empty, and a closed window neither hands its record
    /// over nor takes a merge.
    #[test]
    fn a_workers_window_reaches_its_owner_only_by_take_and_merge() {
        assert!(reset_entered_anchors(), "the owner's window opens");
        let taken = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    // An earlier window on this thread, closed the way a step closes
                    // one: its record stays readable and is no later work's.
                    assert!(reset_entered_anchors(), "an earlier window opens");
                    note_entered("Ring::Pop @ an earlier window");
                    disarm_entered_anchors();
                    assert_eq!(
                        take_entered_anchors(),
                        None,
                        "a closed window hands nothing over"
                    );
                    assert_eq!(entered_anchor_count(), Some(1), "…and keeps its record");
                    assert!(reset_entered_anchors(), "the worker's window opens");
                    note_entered("Ring::Push @ worker");
                    let taken = take_entered_anchors();
                    assert_eq!(window_is_armed(), Some(false), "taking closes it");
                    assert_eq!(entered_anchor_count(), Some(0), "…and empties it");
                    note_entered("Ring::Pop @ worker after the take");
                    assert_eq!(entered_anchor_count(), Some(0), "closed: nothing recorded");
                    taken
                })
                .join()
                .unwrap()
        })
        .expect("the worker's record is taken");
        assert_eq!(taken, BTreeSet::from(["Ring::Push @ worker"]));
        assert_eq!(
            entered_anchor_count(),
            Some(0),
            "the worker's entry never reached the owner by itself"
        );
        note_entered("Ring::Push @ owner");
        assert!(
            merge_entered_anchors(taken),
            "the owner's open window takes it"
        );
        assert_eq!(
            entered_anchor_ids(),
            BTreeSet::from(["Ring::Push @ owner", "Ring::Push @ worker"])
        );
        disarm_entered_anchors();
        assert!(
            !merge_entered_anchors(BTreeSet::from(["Ring::Pop @ late"])),
            "a closed window takes no merge"
        );
        assert_eq!(entered_anchor_count(), Some(2), "and records nothing");
    }
}
