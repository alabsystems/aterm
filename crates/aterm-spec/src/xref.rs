// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The compiler-collectable source↔spec cross-reference (TRUST_NATIVE_TLA, §2).
//!
//! This is the runtime backing for the no-longer-decorative `#[refines]`,
//! `#[spec_unmodeled]`, and `#[spec_invariant]` attribute macros. Each macro now
//! emits an `inventory::submit!` of a record into a distributed slice, so the FULL
//! set of source↔spec bindings is collectable at test/compile time WITHOUT scanning
//! source text — the standard `inventory` pattern.
//!
//! Two halves meet here:
//!   * **Source → spec**: [`RefinementAnchor`] / [`Waiver`] / [`InvariantAnchor`]
//!     records, submitted by the attribute macros, collected via [`refinements`],
//!     [`waivers`], [`invariant_anchors`].
//!   * **Spec → source**: the embedded [`Model`](crate::derive::Model) registry
//!     ([`model_registry`]) + [`Model::anchors`](crate::derive::Model::anchors),
//!     plus external `.tla` parsed by [`TlaSpec::parse`](crate::tla_check::TlaSpec).
//!
//! [`check_closure`] enforces the four bidirectional obligations of §2.2 over both
//! kinds of `SpecModule`, with coverage scoped to *actively-bound* machines (see
//! the doc on that fn). It is the runnable form of `trust-spec-link` (Phase 0),
//! before the IR pass exists (Phase 3).
//!
//! IMPORTANT — collection scope. `inventory` only sees `submit!`s from object code
//! LINKED into the running binary. Anchor-bearing dependencies enable their
//! `spec-anchors` features for the `aterm-gui` unit-test build, whose dependency
//! closure links every participating crate. The full `spec_xref_closure` gate
//! therefore lives in `aterm-gui`; a gate in any individual library would see only
//! that library's own test-expanded anchors.

use std::collections::{BTreeMap, BTreeSet};

use crate::derive::Model;
use crate::tla_check::TlaSpec;

/// A source→spec refinement binding, emitted by `#[refines(machine, action, …)]`.
///
/// This is the `inventory`-collectable anchor record. The struct mirrors
/// [`crate::coverage::RefinementEntry`] but uses `&'static str` so it can live in a
/// `const` submitted at link time (the macro span-captures `file!()`/`line!()`).
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
    /// Optional projection fn path (the `project=` arg), `""` when absent.
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
    /// [`StepAudit::uninstrumented`]), otherwise moving an anchor onto a `const fn`
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

/// A source→spec invariant binding, emitted by `#[spec_invariant(id, machine, …)]`.
/// `machine` is optional for back-compat (`""` when absent); when present it must
/// resolve to a registered `SpecModule` (obligation 4).
#[derive(Debug, Clone, Copy)]
pub struct InvariantAnchor {
    pub machine: &'static str,
    pub id: &'static str,
    pub rust_method: &'static str,
    pub location: &'static str,
}

/// Which verifier discharges a [`ProofAnchor`]'s obligation (TRUST_NATIVE_TLA §4,
/// Phase 4 — "Unify the verifier ledger"). Today only [`Kani`](ProofKind::Kani) is
/// emitted (bounded-local BMC harnesses); the variant exists so the ledger is open to
/// other bounded/SMT verifiers without churning the record shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKind {
    /// A `#[kani::proof]` harness (bounded model checking of a local data-structure
    /// property). Dormant under stock `cargo` (the harness is `#[cfg(kani)]`); the
    /// ANCHOR is decoupled from the harness so it registers in normal/test builds.
    Kani,
}

/// A proof→spec binding, emitted by the `proof_anchor!` macro (TRUST_NATIVE_TLA §4).
///
/// This is the kani-harness analogue of [`RefinementAnchor`]: it joins a bounded-local
/// proof to the SAME `(machine, action)` namespace the temporal (`ty`) models use, so
/// the gate can emit ONE per-action ledger over both verifiers — `ty` (temporal /
/// conformance) and `kani` (bounded-local). No proof is moved or merged; the anchor
/// just records that the named harness *refers to* that action.
///
/// CRITICAL (the §4 subtlety): the kani harnesses are `#[cfg(kani)]`-gated — dormant
/// under stock `cargo`. An ATTRIBUTE on the harness fn would be stripped (and never
/// register) in normal/test builds. So the `proof_anchor!` macro is a MODULE-LEVEL
/// declarative-macro INVOCATION decoupled from the harness fn (it names the harness by
/// string), gated behind the `spec-anchors` feature exactly like [`RefinementAnchor`].
#[derive(Debug, Clone, Copy)]
pub struct ProofAnchor {
    /// The TLA+ machine name (e.g. `"Ring"`) — matches a `Model::name` (lower/CamelCase
    /// resolved by [`machine_matches`]) or an external module name. Same namespace as
    /// [`RefinementAnchor::machine`].
    pub machine: &'static str,
    /// The model action this proof refers to (e.g. `"Push"`) — must resolve in `machine`
    /// (Ob.1), the SAME obligation refinements satisfy.
    pub action: &'static str,
    /// The `#[kani::proof]` harness fn name (e.g. `"line_count_accurate"`). A diagnostic
    /// label for the ledger; the precise DefId binding is out of scope (mirrors
    /// [`RefinementAnchor::rust_method`]).
    pub proof_name: &'static str,
    /// Which verifier discharges this anchor (always [`ProofKind::Kani`] today).
    pub kind: ProofKind,
    /// Source location `file:line` (proc-macro/`file!()`/`line!()` of the invocation).
    pub location: &'static str,
}

inventory::collect!(RefinementAnchor);
inventory::collect!(Waiver);
inventory::collect!(InvariantAnchor);
inventory::collect!(ProofAnchor);

/// All [`RefinementAnchor`]s linked into the current binary.
// Skip: the linkme/`inventory` registry iterator's body lives in the
// third-party `inventory` crate (an absent callee — a static-slice walk
// over link-section entries; no user code, no panic path in practice).
// These accessors are the SPEC-ANCHOR ledger read by the xref gate, not
// shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn refinements() -> impl Iterator<Item = &'static RefinementAnchor> {
    inventory::iter::<RefinementAnchor>.into_iter()
}

/// All [`Waiver`]s linked into the current binary.
// Skip: inventory registry walk — see `refinements()`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn waivers() -> impl Iterator<Item = &'static Waiver> {
    inventory::iter::<Waiver>.into_iter()
}

/// All [`InvariantAnchor`]s linked into the current binary.
// Skip: inventory registry walk — see `refinements()`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn invariant_anchors() -> impl Iterator<Item = &'static InvariantAnchor> {
    inventory::iter::<InvariantAnchor>.into_iter()
}

/// All [`ProofAnchor`]s linked into the current binary (the kani-harness ledger half).
// Skip: inventory registry walk — see `refinements()`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn proof_anchors() -> impl Iterator<Item = &'static ProofAnchor> {
    inventory::iter::<ProofAnchor>.into_iter()
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
// `SelectionCustody` anchor onto `Terminal::is_tmux_mode_active()` — a fn whose
// entire body is `false`, with zero callers in the tree — left the gate GREEN at
// `ratio=1.000 bound=11 waived=0 [ACTIVE]`.
//
// THE MECHANISM. In `cfg(any(test, feature = "spec-anchors"))` builds — the cfg the
// anchor ATTRIBUTE is under — the macro ALSO injects, as the first statement of the
// annotated fn's body, a call to [`note_entered`] with the anchor's
// [`RefinementAnchor::entry_id`].
//
// WHAT THAT CFG GATES, AND WHAT IT DOES NOT, stated exactly because an earlier draft
// of this note said "a shipping binary carries neither the record nor the probe" and
// only the second half of that was true. The cfg gates the ATTRIBUTE, hence the probe
// CALL SITES: a shipping build of `aterm-core` (no `spec-anchors`, not `cfg(test)`)
// has no `note_entered` call in `write_char`. It does NOT gate this module. The two
// thread-local statics below and the four `pub fn`s over them carry no `#[cfg]` at all
// and are compiled into every build of `aterm-spec`, shipping included — they must be,
// because the cfg that strips a probe is the ANNOTATED crate's, and `aterm-spec` cannot
// know at its own compile time whether some downstream crate will emit a call. Whether
// the statics then survive linking with zero callers is a linker-GC question, not a cfg
// one. What the cfg does buy is exactly what the COST paragraph below is about: with no
// call site, no shipping code path pays even the `bool` load.
//
// PER STEP, NOT PER RUN — and that distinction is the whole strength of this. The
// first version of this module audited one window around the WHOLE conformance, which
// asks only "was this fn entered at some point during a run that drives eleven
// actions". That is far weaker than it reads: an anchor moved onto ANY fn the
// conformance happens to touch — `Terminal::process`, or even `Terminal::text_selection`,
// which only the HARNESS'S projection calls — is entered during the run and passes.
// So the window is now ONE STEP wide: [`StepEvidence::step`] clears the record, runs
// the single shipping call that drives action A, and snapshots what was entered before
// the harness reads anything back. `Evict`'s anchor moved onto `Terminal::process`
// then fails, because the `Evict` step calls only `set_scrollback_line_limit`.
//
// AND THE CONTRAPOSITIVE, because per-step entry alone is still weaker than it reads.
// A window is only as narrow as the seam its step calls, and a step that drives a
// `Terminal::process` batch runs the whole VT engine: several anchored functions are
// entered at once and entry cannot tell them apart. `SelectionCustody`'s uniform-scroll
// window enters `Terminal::note_output_custody` as well as `Terminal::post_process`, so
// entry alone would let the anchor sit on either. What separates them is asking the
// other question — a function that is the seam for `{B, C}` must NOT be entered by a
// window driving `D` — and `note_output_custody` also runs in the damage and
// invalidation windows. [`StepAudit::stray`] is that check; [`StepAudit::interchangeable`]
// is what entry alone would have left ambiguous, printed so the difference is visible
// rather than asserted.
//
// THREAD-LOCAL, deliberately. The record is per-thread, so a step's window sees ONLY
// what that step called on the gate's own thread. A process-wide set would be silently
// satisfied by any other test running in parallel in the same binary — which is
// exactly the false green this is here to remove.
//
// ARMED, deliberately. The probe is injected into ~190 bodies tree-wide, several of
// which are per-character hot paths (`Terminal::write_char`) and one of which is inside
// a criterion benchmark's measured region. Outside a step window the probe is a single
// thread-local `bool` load and a predictable branch: [`StepEvidence::step`] arms on
// entry and disarms on exit, and nothing else arms.
//
// WHAT IT IS NOT, stated plainly because the arithmetic matters more than the
// adjective. This is FUNCTION-ENTRY evidence, not per-action BRANCH evidence. A fn
// carrying four anchors marks all four the moment it is entered once, so for those
// four actions the evidence is "the annotated function ran during this action's own
// step" and NOT "the branch that implements this action was taken". Both
// `Terminal::post_process` (4 `SelectionCustody` actions) and
// `Terminal::note_output_custody` (4 `PressCustody` actions) are entered
// unconditionally on every VT batch, so nothing here can tell their siblings apart at
// all — permuting those four labels among themselves changes no observable.
// [`StepAudit::summary`] DERIVES that count and prints it rather than letting the gate
// claim more; [`StepAudit::shared_site`] is the anchor-table list and
// [`StepAudit::indiscriminate`] the run-derived one. The other surviving hole is an
// anchor moved onto a function whose ENTRY PATTERN is exactly the action set it claims
// — including any unanchored function inside the window, which runs no probe and so
// cannot even be counted. Full symbol and branch resolution is still Phase 3's
// `trust-ir` job.

thread_local! {
    /// Is a [`StepEvidence::step`] window open on this thread? The probe's first act
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
/// THE RETURN VALUE IS THE POINT. This used to be `-> ()` with both failure modes
/// (`try_with` on a destroyed TLS slot, `try_borrow_mut` on a re-entrant borrow)
/// discarded, so a reset that silently no-opped left the PREVIOUS conformance's
/// entries in place and every audit after it passed on stale evidence — a total
/// fail-open, and invisible. A caller that cannot make the window open must fail
/// loudly instead; [`StepEvidence::step`] asserts on it.
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
/// observe, so [`StepEvidence::step`]'s post-reset check can be genuinely independent
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
/// Exists so [`StepEvidence::step`] can check its window SURVIVED the closure it ran.
/// The exit disarms unconditionally, so a nested `step` — or any hand call to
/// [`disarm_entered_anchors`] — inside the drive closure closes the outer window early,
/// and the outer action is then credited with whatever the inner window recorded. The
/// non-nesting rule used to be a doc comment with nothing behind it.
// Skip: thread-local read (`LocalKey::try_with`, `Cell::get`). Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn window_is_armed() -> Option<bool> {
    ARMED.try_with(core::cell::Cell::get).ok()
}

/// What one conformance STEP entered: the action the step claims to drive, and the
/// anchor ids whose annotated fn ran inside that step's window and no wider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    /// The machine action this step drives, spelled as the anchors spell it.
    pub action: &'static str,
    /// Anchor ids entered between the window opening and closing.
    pub entered: BTreeSet<&'static str>,
}

/// A machine's per-step execution evidence, accumulated by its Tier-1 conformance and
/// audited by the gate that ran it.
///
/// The conformance owns this because only the conformance knows where one step ends
/// and the next begins — which is precisely the knowledge that makes the evidence
/// discriminating rather than cumulative.
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

    /// Drive ONE step of the conformance inside its own evidence window.
    ///
    /// `drive` must be the shipping call for `action` and NOTHING ELSE — no pre-state
    /// read, no post-state projection, no record read-back. Everything inside the
    /// window becomes evidence for `action`, so a window that also contains the
    /// harness's own `term.text_selection()` would let an anchor parked on that
    /// accessor discharge a shipping obligation.
    ///
    /// Windows MUST NOT NEST: the exit disarms unconditionally, so an inner step would
    /// close the outer one's window early and silently lose evidence. That rule is now
    /// ENFORCED rather than documented — the arm bit is re-read after `drive` returns.
    ///
    /// # Panics
    /// If the window cannot be opened (see [`reset_entered_anchors`]), if the record is
    /// not observably empty once it has (see [`entered_anchor_count`]), or if the
    /// window was closed while `drive` ran (see [`window_is_armed`]). A silent no-op
    /// reset would leave the previous step's entries in place and make every subsequent
    /// audit pass on stale evidence, so all three are deliberately loud.
    // Skip: thread-local window bookkeeping + `Vec::push` (absent std bodies), around
    // a caller-supplied closure. Verification tooling, like the rest of this module.
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

    /// The recorded windows, in the order they were driven.
    #[must_use]
    pub fn records(&self) -> &[StepRecord] {
        &self.steps
    }

    /// The machine this ledger is about.
    #[must_use]
    pub fn machine(&self) -> &'static str {
        self.machine
    }

    /// Cross the ledger with the machine's linked anchors.
    // Skip: set algebra over the inventory walk (BTreeSet/Map builds, absent std
    // bodies). Verification tooling, like the rest of this module.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn audit(&self) -> StepAudit {
        audit_steps(self.machine, &self.steps)
    }
}

/// The execution-evidence verdict for one machine, crossing its linked anchors with a
/// [`StepEvidence`] ledger.
///
/// Every field is DERIVED from the anchors and the ledger. Nothing here is a number
/// the gate asserts in prose — including the honest limits, which is the point of
/// [`Self::shared_site`], [`Self::interchangeable`] and [`Self::entered_under`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAudit {
    /// The machine, spelled as the caller spelled it.
    pub machine: String,
    /// How many of the machine's anchors are LINKED into this binary (probed or not).
    pub anchors: usize,
    /// The distinct ACTIONS those anchors cover. The gate asserts this SET, not a
    /// count: the design permits several anchor sites per action, so an anchor count
    /// would turn a legitimate second site into a bogus "linkage" failure.
    pub actions: BTreeSet<&'static str>,
    /// How many step windows the conformance recorded.
    pub steps: usize,
    /// Anchors the macro could not probe (`entry_id == ""`), named by `rust_method`.
    /// A failure for a machine claiming execution evidence — otherwise moving an
    /// anchor onto a `const fn` would be a way to opt back out of the check.
    pub uninstrumented: Vec<&'static str>,
    /// `entry_id`s claimed by more than one anchor, which would let one fn's entry
    /// discharge another's obligation.
    pub ambiguous: Vec<&'static str>,
    /// Anchored actions with NO step in the ledger: the conformance never claimed to
    /// drive them, so nothing was checked about their anchors.
    pub undriven: Vec<&'static str>,
    /// Anchored actions that WERE driven and whose annotated function was entered in
    /// no step of their own. THE FAILURE LIST — an anchor on a stub, on a fn the
    /// action's own step never reaches, or on an unrelated fn (however busy that fn is
    /// elsewhere in the run) lands here.
    pub unwitnessed: Vec<&'static str>,
    /// One entry per WINDOW that did not enter its own action's function, named by that
    /// window's action (so an action driven three times can appear twice).
    ///
    /// [`Self::unwitnessed`] is an OR across an action's windows, so `Evict` — driven
    /// by three steps — passes on any one of them and the other two are unchecked. This
    /// is the AND that closes that: it is the difference between
    /// [`Self::witnessed_steps`] and the windows that name a real action.
    pub blind_steps: Vec<&'static str>,
    /// Step tags that match no anchored action of this machine — a typo in the
    /// conformance, checking nothing.
    pub unanchored_steps: Vec<&'static str>,
    /// Annotated fn → the actions of this machine anchored on it.
    pub by_function: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Actions sharing their annotated fn with at least one sibling action. Entry
    /// alone CANNOT tell these apart: one entry marks every anchor on that fn.
    ///
    /// Derived from the ANCHOR TABLE, not from the run — the execution-derived twin is
    /// [`Self::indiscriminate`], and the two are not the same question.
    pub shared_site: BTreeSet<&'static str>,
    /// Action → the already-anchored FUNCTIONS that every window driving that action
    /// entered. THE HONEST DENOMINATOR of what a window pins: `witnessed` requires the
    /// action's own function to be in this set, and an anchor moved onto ANY other
    /// member of it would be witnessed exactly the same. `1` is a window that pins the
    /// action to its own function; `k` is a window that cannot tell it from `k - 1`
    /// others.
    ///
    /// A LOWER BOUND on the real ambiguity, and that has to be said wherever the number
    /// is printed: only a function that ALREADY carries an anchor runs a probe, so an
    /// unanchored function the same window calls is invisible here and would also pass.
    pub interchangeable: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Anchored function of this machine → the actions whose windows entered it.
    ///
    /// THE REPLACEMENT FOR `ubiquitous`, which asked for a function entered by
    /// LITERALLY EVERY window and was therefore structurally empty on any ledger that
    /// mixes step families (GUI gesture, VT batch, eviction) — i.e. on both real ones.
    /// It printed `0` and read like an all-clear for exactly the population
    /// (`Terminal::post_process`, `Terminal::note_output_custody`) the module prose
    /// correctly describes as entered on every batch regardless of what fired. A
    /// function here with more than one action is entered under all of them, which is
    /// the vacuous-discharge fact stated in a form that can actually be non-empty.
    pub entered_under: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Actions at least one of whose anchored functions was entered by the window of
    /// some OTHER action of this machine — so entry says the function ran, not which
    /// action fired. The execution-derived twin of [`Self::shared_site`].
    ///
    /// On a ledger with [`Self::stray`] and [`Self::blind_steps`] both empty this set
    /// is EQUAL to `shared_site`, which is worth printing: one is read off the anchor
    /// table and the other off the run, and their agreeing is a check that the table
    /// describes what actually happened.
    pub indiscriminate: BTreeSet<&'static str>,
    /// Anchored function of this machine → the actions whose windows entered it that
    /// this function is NOT anchored for.
    ///
    /// THE WITHIN-WINDOW DETECTOR, and the reason the per-step check discriminates
    /// INSIDE a `Terminal::process` batch rather than only across batches.
    /// [`Self::unwitnessed`] asks whether a function ran when its own action fired;
    /// this asks the contrapositive — did it run when something ELSE fired? A function
    /// that is the seam for `{B, C}` has no business being entered by a window driving
    /// `D`. So an anchor moved onto a different function inside the same window is
    /// caught exactly when that function's ENTRY PATTERN is wider than the action set
    /// it now claims: park `SelectionCustody::UniformScroll` on
    /// `Terminal::note_output_custody` — which the uniform-scroll batch really does
    /// enter — and it lands here, because that function also runs in the two damage
    /// windows and the two invalidation windows.
    ///
    /// IT CAN FIRE ON A STRUCTURALLY-SHARED FUNCTION rather than a misplaced anchor: a
    /// function entered on every batch, in a machine that also drives a batch-shaped
    /// action it is not anchored for. That is not a false alarm about the code — it is
    /// the statement that such a function's entry is no evidence for its own actions —
    /// and the resolutions are to anchor it for that action too (if the seam really is
    /// shared), to narrow the window, or to move the anchor to the seam that is
    /// specific.
    pub stray: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Per step window, in the order driven: the action it drives and the distinct
    /// already-anchored FUNCTIONS it entered. A window with one function pins its
    /// action to that function; a window with `k` — a whole `Terminal::process` batch,
    /// say — ran `k` anchored functions and this check can tell none of them apart.
    /// Same LOWER-BOUND caveat as [`Self::interchangeable`]: an unanchored function in
    /// the same window runs no probe and cannot appear here.
    pub window_reach: Vec<(&'static str, BTreeSet<&'static str>)>,
    /// Anchor SITES of a MULTI-SITE action that no window of that action ever entered.
    ///
    /// `witnessed` is an OR across sites, so the moment an action has two the second is
    /// never required to run at all. Single-site actions are excluded because for them
    /// this is just [`Self::unwitnessed`] under another name.
    pub dark_sites: Vec<&'static str>,
    /// How many anchor sites belong to an action that has MORE THAN ONE — the
    /// DENOMINATOR of [`Self::dark_sites`], carried so a printed `0 dark sites` cannot
    /// read as an all-clear when the real reason is that no action has a second site
    /// yet. (That confusion is exactly what the old `ubiquitous` field did.)
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

    /// How many windows entered MORE THAN ONE anchored function — the windows inside
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

    /// The honest arithmetic, derived — what the evidence pins and what it does not, in
    /// numbers rather than adjectives.
    ///
    /// EVERY NUMBER HERE IS A FACT ABOUT THE RUN, not a quality score, and the output
    /// says so in its own last clause. Two of them — `shared_site` and the count of
    /// `indiscriminate` actions — go DOWN when an anchor is moved somewhere wrong,
    /// because a wrong function carries fewer of the machine's actions than the right
    /// one did. Take an action off a four-action seam and park it on a function only
    /// its own window enters: [`Self::stray`] cannot object (that function's entry
    /// pattern IS the action set it now claims), the ledger stays green, and the
    /// shared-function figure falls from ten to nine. Reading either number as
    /// "discrimination improved" is exactly the mistake this whole module exists to
    /// stop, so both are labelled for what they are.
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

/// The annotated FUNCTION half of an entry id (`"<machine>::<action> @ <fn>"`).
///
/// The id names an ANCHOR, but what a window observes is the FUNCTION: two anchors on
/// one fn produce two ids that always fire together, and an anchor moved elsewhere
/// produces a different id only because this half changed. So every question of the
/// form "what else could this anchor have sat on and still been witnessed?" is a
/// question about this half, which is why [`StepAudit::interchangeable`] and
/// [`StepAudit::entered_under`] are keyed by it rather than by the whole id.
fn fn_of(entry_id: &'static str) -> &'static str {
    entry_id.rsplit_once(" @ ").map_or(entry_id, |(_, f)| f)
}

/// Cross `machine`'s linked anchors with a per-step ledger. `machine` is resolved with
/// [`machine_matches`], so the caller may spell it the way the anchors do.
// Skip: inventory walk feeding the pure set algebra below. Verification tooling.
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
// Skip: set algebra (BTreeSet/Map builds, absent std bodies). Verification tooling,
// like the rest of this module.
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

    // The AND that `unwitnessed`'s OR leaves open: an action driven by three steps is
    // `witnessed` on any ONE of them, so the other two check nothing until this names
    // them. (`unanchored_steps` is reported separately; a mistyped tag is not a blind
    // window, it is a window about no action at all.)
    let blind_steps: Vec<&'static str> = steps
        .iter()
        .filter(|s| actions.contains(&s.action) && !witnessed_of(s))
        .map(|s| s.action)
        .collect();

    // What each window entered, as FUNCTIONS: the set an anchor may be moved anywhere
    // within and still be witnessed.
    let fns_of_step: Vec<BTreeSet<&'static str>> = steps
        .iter()
        .map(|s| s.entered.iter().copied().map(fn_of).collect())
        .collect();

    // Per action, the functions EVERY one of its windows entered. This is the honest
    // denominator of the per-step check: `witnessed` puts the action's own function in
    // here, and any other member would have passed identically.
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

    // Per anchored function of THIS machine, the actions whose windows entered it —
    // the non-vacuous form of the old `ubiquitous` (see `StepAudit::entered_under`).
    let mut entered_under: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    for (s, fns) in steps.iter().zip(&fns_of_step) {
        if !actions.contains(&s.action) {
            continue;
        }
        for f in fns.iter().copied().filter(|f| by_function.contains_key(f)) {
            entered_under.entry(f).or_default().insert(s.action);
        }
    }

    // An action is indiscriminate when SOME function it is anchored on was entered by
    // SOME other action's window: the OR across sites means that site alone could have
    // discharged it, so entry says the function ran and not which action fired.
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

    // Entered under an action this function is NOT anchored for: the within-window
    // detector (see `StepAudit::stray`).
    let stray: BTreeMap<&'static str, BTreeSet<&'static str>> = entered_under
        .iter()
        .filter_map(|(f, under)| {
            let own = by_function.get(f)?;
            let extra: BTreeSet<&'static str> = under.difference(own).copied().collect();
            (!extra.is_empty()).then_some((*f, extra))
        })
        .collect();

    // Sites of a MULTI-site action that no window of that action entered, and how many
    // sites are eligible to land there at all.
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

/// Every embedded [`Model`] aterm-spec knows about — the spec→source registry.
///
/// This enumerates ALL `ty_model!`/`derive`-authored models so the closure gate can
/// resolve a `machine` named by an anchor to a registered `SpecModule` (obligation
/// 4) and enumerate a machine's actions (obligation 1 + coverage). Adding a model
/// here makes it a first-class anchor target.
// Skip: builds the model registry by calling the (T2-classified) `*_model()`
// data constructors — the vec! alloc + their absent bodies. Spec tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn model_registry() -> Vec<Model> {
    use crate::derive::*;
    vec![
        terminal_modes_model(),
        ring_model(),
        cursor_model(),
        subscribe_model(),
        transact_model(),
        kernel_model(),
        snapshot_model(),
        read_image_seq_model(),
        // Resident operator safety: durable event claims, guarded mutation WAL +
        // attempted-input epoch, GAP/resnapshot cursors, single-leader epoch
        // fencing, and the durable fleet-fault gate. Tier-1 binds these scalar
        // projections to shipping reducers.
        operator_event_delivery_model(),
        operator_wal_actuator_model(),
        operator_resync_cursor_model(),
        operator_leadership_model(),
        operator_fleet_fault_model(),
        // A7 (WS-G): the PTY-master fd-lifecycle ownership discipline — drift-free
        // twin of FdLifecycle.tla, anchored to aterm-session/src/sink.rs.
        fd_lifecycle_model(),
        // WS-G: spawn-time locale guarantee — the child always runs under a UTF-8
        // LC_CTYPE. Abstract twin of aterm_pty::resolve_spawn_locale (real-code
        // binding in aterm-pty's spawn_locale_conformance test). Proves-and-catches.
        spawn_locale_model(),
        evict_full_model(),
        tier_residency_model(),
        recording_model(),
        coalesce_model(),
        window_routing_model(),
        // SELECTION CUSTODY: who owns the reading position and the highlight.
        //
        // `SelectionCustody` is ACTIVELY-BOUND — 11/11 actions anchored, Tier-1
        // conformance driving real gestures and real `Terminal::process` batches
        // (`aterm_gui::selection_custody_conformance`), run by the gate.
        //
        // `PressCustody` is ACTIVELY-BOUND too, as of the custody RECORD. It was
        // report-only for a real reason rather than a procedural one: five of its
        // eleven actions had no seam at which their state change could be told from
        // another's — `RepeatPress`, `InertPress` and `ReleaseEvent` are identical in
        // every observable variable, and `OutputAtLive` is an identity transition any
        // function satisfies — so anchoring bought a green ledger line rather than a
        // check. `Terminal::note_custody` closes that: the site that DECIDES a
        // transition records which one it was, so the conformance validates each step
        // against the action the engine itself named. 11/11 anchored on the four
        // recorders, Tier-1 in `aterm_gui::press_custody_conformance`, run by the gate.
        //
        // Registration alone (independent of anchoring) is what puts BOTH under
        // `ty check --strict-vacuity` in the repo-wide gate.
        press_custody_model(),
        selection_custody_model(),
        // Introspection / recursive-stacking control plane (audit findings M1/M2/S1).
        dispatch_complete_model(),
        relay_teardown_model(),
        proxy_registry_model(),
        // Liveness twin: forward-handshake deadlock-freedom (the drain_buffered class).
        forward_handshake_model(),
        // TLS-specific bind of the same no-fresh-read-before-buffer-drain wedge.
        tls_buffered_relay_model(),
        // Generalized error-class models (F1 info-flow, ordering, reply-fidelity).
        capability_secrecy_model(),
        publish_ordering_model(),
        reply_fidelity_model(),
        // Capability-layer audit: the trust core's authorization-soundness predicate.
        authorize_soundness_model(),
        // Deep-nesting safety: forwarding needs Owner scope (no transitive authority).
        no_transitive_authority_model(),
        // GUI native-chrome safety: split-pane tree integrity + session-pool refcount
        // accounting (the Tier-1 conformance + #[refines] anchors live in aterm-gui).
        pane_tree_model(),
        session_pool_model(),
        // Native titlebar tab-strip parity: the NSSegmentedControl mirror discipline
        // (seg_count==count, selected==active). Tier-1 conformance + #[refines] anchors
        // live in aterm-gui (projects the strip lane from WindowState::strip_shadow).
        tab_strip_model(),
        // Native first-party tab apps: Tier-0 platform contracts from
        // docs/NATIVE_TAB_APPS_DESIGN.md section 8. Tier-1 is deliberately added only
        // with each genuine shipping service; registering the models here makes the
        // drift-free spec source discoverable to the closure/anchor ledger today.
        control_connection_admission_model(),
        native_control_routing_model(),
        native_tab_identity_model(),
        native_reopen_ledger_model(),
        closed_recovery_ledgers_model(),
        native_settings_singleton_model(),
        native_settings_draft_close_model(),
        manual_config_handoff_model(),
        native_packages_worker_model(),
        native_markdown_history_model(),
        native_markdown_viewport_model(),
        native_editor_viewport_model(),
        native_editor_command_palette_model(),
        native_recovery_interaction_model(),
        native_editor_modal_model(),
        native_config_transaction_model(),
        serious_mode_intent_queue_model(),
        config_file_commit_cas_model(),
        config_catalog_snapshot_model(),
        composite_accessibility_route_model(),
        native_document_publication_model(),
        native_draft_journal_model(),
        restore_manifest_single_use_model(),
        native_close_plan_model(),
        native_save_intent_latch_model(),
        native_async_delivery_model(),
        // Smart terminal-title summarization: each session's coalesced async slot is
        // bound to the latest content + settings generations, including
        // disable/re-enable; companions prove two-session fairness, timing,
        // cancellation, owned worker/runtime lifecycle, distinct per-process
        // managed endpoints with revocation-safe health telemetry, and bounded
        // retry of transient multi-owner socket snapshots without relaxing the
        // exact-owner decision.
        title_summary_model(),
        title_summary_observation_scheduler_model(),
        title_summary_runtime_model(),
        title_summary_managed_endpoint_model(),
        title_summary_socket_owner_retry_model(),
        native_updater_model(),
        // Release/updater channel state machines. The release-floor resolver and
        // journal/guard Tier-1 live in aterm-release; the archive model is the
        // metadata-only single-head lifecycle; the updater scan binding lives in
        // aterm-update::github.
        release_durable_post_intent_model(),
        release_channel_floor_model(),
        release_journal_prefix_model(),
        release_publisher_fence_model(),
        release_key_epoch_transition_model(),
        release_historical_recovery_model(),
        release_published_identity_model(),
        release_yank_successor_first_model(),
        release_channel_single_head_model(),
        native_update_channel_scan_model(),
        native_update_admission_model(),
        native_update_auto_intent_model(),
        native_update_hidden_output_quiet_model(),
        native_update_attempt_identity_model(),
        native_update_menu_activation_model(),
        native_update_worker_queue_model(),
        native_update_status_reconciliation_model(),
        // The FailedMark writer/reader suppression contract (the 5ffcc15d
        // crash-loop poison class). Tier-1 conformance + #[refines] anchors
        // live in aterm-update::manifest.
        native_update_failed_mark_suppression_model(),
        trail_audio_lifecycle_model(),
        trail_audio_start_latency_model(),
        asymmetric_pad_layout_model(),
        visible_pad_crop_model(),
        focus_modifier_cache_model(),
        input_release_pairing_model(),
        tab_stop_handoff_model(),
        scrollback_maintenance_lane_model(),
        top_anchored_scroll_history_model(),
        kitty_sing_detector_model(),
        cursor_cat_earn_floor_model(),
        cursor_cat_curse_wince_model(),
        reduced_motion_companion_handoff_model(),
        cursor_cat_motion_pulse_routing_model(),
        cursor_hint_license_model(),
        cursor_viewport_lifecycle_model(),
        // Resident cursor companions retain personality across a pane/tab
        // owner edge, but never coordinates or hit targets. A truly
        // unpresentable focus loss has the same retirement law; recording and
        // typed-wake focus pins are modeled as explicit preservation controls.
        // Tier-1 drives the real sync_window/on_focus decisions in aterm-gui.
        cursor_companion_owner_lifecycle_model(),
        composed_sync_hold_model(),
        sync_reopen_visibility_model(),
        cursor_effect_scroll_model(),
        cursor_scroll_signal_model(),
        rainbow_jump_burst_lifecycle_model(),
        rainbow_terminus_admission_model(),
        native_update_overlap_handoff_model(),
        native_update_disk_transaction_model(),
        exact_profanity_completion_model(),
        settings_page_scroll_model(),
        // Fixed-path snapshot publication is generation fenced: once a newer
        // request begins, an overtaken encoder cannot publish stale payload or
        // its completion marker. Tier-1 binds the real path-generation fence in
        // aterm-gui/src/app_introspect.rs.
        snapshot_generation_commit_model(),
        // A video request owns its pre-created private directory from admission
        // through recording/export. All abort paths clean it, success transfers
        // it to publication, and the recording/export slots never overlap.
        // Tier-1 binds the real GUI lifecycle and process-wide export permit.
        video_recording_lifecycle_model(),
        // Per-process media retention prefers an exact lease observation over
        // numeric PID liveness; only a legacy namespace without a lease may use
        // the PID fallback. Tier-1 binds the GUI's full lease×PID decision.
        exact_instance_retention_model(),
        // Confined artifact I/O retains the original inside object across
        // ancestor swaps and validates its identity again before replying.
        // Tier-1 binds GUI/media read and write transactions.
        anchored_artifact_transaction_model(),
        // Capture publication continues past worker enqueue: publication guards
        // are retained and revalidated through the control socket's complete reply
        // and a fresh causal nonce challenge/echo. ACK-error/half-closed clients
        // and partial write failures retain the guard in a central quarantine
        // until its abstract 30-second expiry; only a valid echo releases
        // immediately.
        artifact_reply_publication_model(),
        // Failed ACKs can outlive their control workers, so one process-wide
        // admission cap spans queued, active, and quarantined artifact replies.
        artifact_handoff_capacity_model(),
        // Every video frame/index member is file-synced before its write action;
        // one directory batch barrier must cover the complete current member set
        // before the reader-visible publication marker can appear. Tier-1 binds
        // all three transitions to ConfinedVideoDir.
        video_batch_publication_durability_model(),
        // The video producer and `video frames` readers share a bounded lease
        // count. Marker publication or final reader validation requests one
        // capability-bound last-release sweep; acquisition is fail-closed from
        // the last-release decision through sweep completion.
        artifact_reader_lease_model(),
        capture_after_present_model(),
        native_capture_source_model(),
        // Presented destination capture: one-shot serial-bound lifecycle plus
        // the streaming staging-slot reuse/drop and sequence-ordered harvested
        // store disciplines. Shipping pure transition gates and Tier-1 negative
        // controls live in aterm-gpu.
        presented_frame_tap_model(),
        video_tap_slot_model(),
        // HDR reconfigure/live-validation lifecycle: an f16 DX12 swapchain
        // remains HDR only after its scRGB re-tag/check succeeds; failure
        // atomically selects SDR and reconciles capture metadata. Tier-1 lives
        // in aterm-gpu/tests/hdr_gate.rs.
        hdr_reconfigure_retag_model(),
        layout_coordinate_reset_model(),
        semantic_prewarm_generation_model(),
        semantic_prewarm_handshake_model(),
        semantic_prewarm_request_swap_model(),
        // 2026-07-20 zoom/typing incident: whole-surface presentation,
        // bounded/typed recovery, redraw delivery, and no-flash predictive
        // visibility. These are scalar prove-and-catch models, so registering
        // them enrolls every action in the global strict-vacuity audit as well
        // as making them first-class spec-link targets.
        surface_coverage_model(),
        startup_phase_publication_model(),
        present_retry_model(),
        gpu_loss_route_model(),
        gpu_loss_recovery_model(),
        recovery_redraw_model(),
        predictive_echo_visibility_model(),
        // Streaming-search lifecycle: drift-free twin of aterm-search's
        // StreamingSearch engine (supersedes the never-committed hand
        // StreamingSearch.tla). Registering it enrolls every action in the
        // global strict-vacuity audit, the verifier ledger, and trust-ir
        // spec-link; #[refines] anchors + Tier-1 lockstep live in aterm-search.
        streaming_search_model(),
        // Budgeted full-buffer search's owner-level resume capability. This is
        // distinct from StreamingSearch: it proves reset + fresh search identity
        // across restart classes, cursor retirement, one-turn completion, and
        // scan-complete result-delta draining without row replay. #[refines]
        // anchors live on Terminal's public calls; Tier-1 drives real returned
        // steps in aterm-core/tests/conformance_budgeted_search.rs.
        budgeted_search_resume_model(),
        // Host-minted OSC-8 hyperlink scheme capability (orca deep-links §7):
        // bounded extra-scheme set, never-allow refusal, revoke restores the
        // default allowlist. #[refines] anchors live on
        // Terminal::authorize_hyperlink_scheme / revoke_hyperlink_scheme and
        // the OSC-8 acceptance handler; Tier-1 conformance in aterm-core
        // (conformance_hyperlink_scheme_cap.rs).
        hyperlink_scheme_cap_model(),
    ]
}

/// The action/anchor name set of an embedded [`Model`] — for embedded models the
/// invariant-def and coverage-action namespaces coincide, so both
/// [`SpecModule::invariant_names`] and [`SpecModule::coverage_actions`] use this.
// Explicit insert loop (not `collect`) so the Trust L0 allocation check sees
// per-element growth instead of an unbounded bulk allocation.
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
/// Obligation 4 (machine resolves) is satisfied by EITHER variant.
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

    /// The names a REFINEMENT / PROOF / WAIVER anchor may RESOLVE to for obligation 1
    /// ("action exists"). This is the ACTION namespace — for an embedded model its
    /// `Action` names; for an external `.tla` the `Next` disjuncts ONLY (the
    /// [`coverage_actions`](Self::coverage_actions) set), NOT every top-level def.
    ///
    /// TRUST_VACUITY_GATE §2.4 (finding 4): the External arm previously returned
    /// `t.actions` (ALL top-level defs — `Init`/`TypeOK`/named constants/invariants),
    /// so an external `#[refines]`/`proof_anchor!` aimed at a non-`Next` def like
    /// `TypeOK` OVER-resolved and was wrongly accepted. The lowered Trust artifact is
    /// already strict here (it emits only `coverage_actions()` and Ob.1-checks against
    /// that — the L3 lock), so the in-Rust gate was the looser of the two. Narrowing
    /// this to `coverage_actions()` ALIGNS the in-Rust gate with Trust's
    /// already-strict artifact: a `#[refines]`/`proof_anchor!` naming `Init`/`TypeOK`
    /// now fails Ob.1 in BOTH paths. `#[spec_invariant]` keeps the full def set via the
    /// separate [`invariant_names`](Self::invariant_names) (invariants legitimately
    /// name non-`Next` defs).
    pub fn action_names(&self) -> BTreeSet<String> {
        // For an embedded model the action set IS the coverage set; for an external
        // `.tla` this is now the `Next` disjuncts only (the L3-locked behavior).
        self.coverage_actions()
    }

    /// The names a `#[spec_invariant]` `id` may RESOLVE to (obligation 1 for the
    /// INVARIANT arm ONLY). This is the full top-level definition set — for an
    /// embedded model its action names; for an external `.tla` EVERY top-level
    /// definition (`t.actions`), because an invariant legitimately names a non-`Next`
    /// def like `TypeOK`/`Confined`.
    ///
    /// TRUST_VACUITY_GATE §2.4: this is the deliberately-WIDER set used ONLY by the
    /// `#[spec_invariant]` id arm, kept separate from [`action_names`](Self::action_names)
    /// (the Next-only set the refinement/proof/waiver arms use) so narrowing the action
    /// set does not break invariants that name `TypeOK`-style defs.
    // Skip: `BTreeSet`/`BTreeMap` keyed build + iterator `next` (absent std
    // bodies). Spec-anchor ledger machinery read by the xref gate.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn invariant_names(&self) -> BTreeSet<String> {
        match self {
            SpecModule::Embedded(m) => embedded_action_set(m),
            SpecModule::External(t) => t.actions.clone(),
        }
    }

    /// The real ACTION set used for the COVERAGE obligation (obligation 3): every
    /// action must be bound-or-waived for an actively-bound machine. For an embedded
    /// model this is its `Action` names (same as [`action_names`](Self::action_names)).
    /// For an external `.tla` it is the disjuncts of `Next == …` ONLY — NOT every
    /// top-level def, so coverage never demands a `#[refines]` for `Init`/`Spec`/
    /// `TypeOK`/an invariant/a named constant (which are not actions). When a spec has
    /// no parseable `Next` disjuncts (defensive), fall back to the full def set so
    /// coverage cannot be vacuously satisfied.
    // Skip: BTreeSet keyed build + iterator `next` (absent std bodies) —
    // same class as `invariant_names`. Spec-coverage machinery.
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

/// Whether an anchor's `machine` string resolves to a `SpecModule` named `name`.
///
/// Anchors use a lower_snake/lowercase convention (`"terminal_modes"`,
/// `"window_routing"`, `"ring"`) while a `Model::name`/MODULE is CamelCase
/// (`"TerminalModes"`, `"WindowRouting"`, `"Ring"`). We match case-insensitively
/// after stripping `_`, so `"terminal_modes"` ⟺ `"TerminalModes"`.
pub fn machine_matches(anchor_machine: &str, module_name: &str) -> bool {
    // Skip: char-filter iterator `next` + String alloc (absent std bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn norm(s: &str) -> String {
        // Explicit push loop (not `collect`) so the Trust L0 allocation check sees
        // per-element growth instead of an unbounded bulk allocation;
        // behavior-identical.
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
    /// Which of the four §2.2 obligations was violated (1, 3, or 4 here; 2 —
    /// "symbol resolves to a live DefId" — needs the Trust IR lowering of Phase 3,
    /// so it is out of scope for the Phase-0 aterm-local gate and not asserted).
    /// See [`StepEvidence`] for the executable fragment of obligation 2 that the
    /// conformance-owning gates DO assert: per-STEP function-entry evidence, which
    /// is strictly weaker than symbol resolution and says so in its own summary.
    pub obligation: u8,
    pub message: String,
}

/// A per-machine coverage line for the report (printed for every active machine,
/// and surfaced for the non-active embedded models so ratios are visible — the
/// "REPORT the rest" half of obligation 3).
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
        // Saturating: `bound` and `waived` are disjoint in-memory sets, so their
        // combined size can never exceed `usize::MAX` — saturation is a no-op on
        // every real input; it only discharges the unconstrained-input overflow
        // obligation (Trust L0).
        let covered = self.bound.len().saturating_add(self.waived.len());
        // `max(1)`: `total_actions == 0` already returned above, so the clamp is
        // a no-op on every reached input; it only discharges the
        // unconstrained-divisor obligation (Trust L0).
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

/// One row of the UNIFIED VERIFIER LEDGER (TRUST_NATIVE_TLA §4, Phase 4): for a single
/// `(machine, action)`, which verifier(s) discharge it — `ty` (temporal: a `#[refines]`
/// binding drives Tier-0/Tier-1 `ty` over the model action) and/or `kani` (bounded-local:
/// a `proof_anchor!`'d harness refers to it). This is the "single coverage ledger over
/// both verifiers" the design calls for — no proof is merged; the IR just learns they
/// refer to the same action.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    /// Canonical machine name (the resolved `SpecModule.name`).
    pub machine: String,
    /// The model action.
    pub action: String,
    /// Discharged temporally: ≥1 `#[refines]` (or a `#[spec_unmodeled]` waiver) binds
    /// this action to the `ty`-checked model.
    pub ty: bool,
    /// Discharged bounded-locally: ≥1 `proof_anchor!`'d kani harness refers to it.
    pub kani: bool,
    /// The kani harness names that refer to this action (for the report detail).
    pub proofs: BTreeSet<String>,
}

impl LedgerEntry {
    /// The `ty=✓/–  kani=✓/–  <machine>::<action>` line for the gate report.
    pub fn render(&self) -> String {
        let mark = |b: bool| if b { "✓" } else { "–" };
        format!(
            "ty={}  kani={}  {}::{}",
            mark(self.ty),
            mark(self.kani),
            self.machine,
            self.action
        )
    }
}

/// Build the per-`(machine, action)` UNIFIED VERIFIER LEDGER over the registered
/// `modules`, the collected `refinements`/`waivers` (the `ty`/temporal half) and
/// `proof_anchors` (the `kani`/bounded-local half) (TRUST_NATIVE_TLA §4, Phase 4).
///
/// Every action of every registered machine gets a row; `ty` is set iff a refinement
/// (or a machine+action waiver) binds it, `kani` iff a `proof_anchor!`'d harness names
/// it. Anchors whose `machine` resolves to no module are silently skipped here — they are
/// already flagged as Ob.4 violations by [`check_closure`], so the ledger never invents a
/// row for a dangling machine.
// Skip: BTreeSet keyed build + Extend/format (absent std bodies). The
// spec-anchor ledger the xref gate reads; not shipping runtime code.
#[cfg_attr(trust_verify, trust::skip)]
pub fn verifier_ledger(modules: &[SpecModule]) -> Vec<LedgerEntry> {
    // Seed a row for every (canonical machine, action).
    let mut rows: BTreeMap<(String, String), LedgerEntry> = BTreeMap::new();
    for m in modules {
        let canon = m.name().to_string();
        for action in m.action_names() {
            rows.entry((canon.clone(), action.clone()))
                .or_insert_with(|| LedgerEntry {
                    machine: canon.clone(),
                    action: action.clone(),
                    ty: false,
                    kani: false,
                    proofs: BTreeSet::new(),
                });
        }
    }

    let resolve = |anchor_machine: &str| -> Option<String> {
        modules
            .iter()
            .find(|m| machine_matches(anchor_machine, m.name()))
            .map(|m| m.name().to_string())
    };

    // ty half: refinements and machine+action waivers mark `ty` for their action.
    let mut mark_ty = |machine: &str, action: &str| {
        if let Some(canon) = resolve(machine)
            && let Some(e) = rows.get_mut(&(canon, action.to_string()))
        {
            e.ty = true;
        }
    };
    for r in refinements() {
        mark_ty(r.machine, r.action);
    }
    for w in waivers() {
        if w.machine.is_empty() || w.action.is_empty() {
            continue;
        }
        mark_ty(w.machine, w.action);
    }

    // kani half: each proof anchor marks `kani` and records the harness name.
    for p in proof_anchors() {
        if let Some(canon) = resolve(p.machine)
            && let Some(e) = rows.get_mut(&(canon, p.action.to_string()))
        {
            e.kani = true;
            e.proofs.insert(p.proof_name.to_string());
        }
    }

    rows.into_values().collect()
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

/// Shared Ob.4 ("machine exists") + Ob.1 ("action/id exists") arm for one anchor:
/// push a [`ClosureViolation`] when `resolved` is `None`, else when `target` does
/// not resolve in the namespace `select`ed from the module's index. `site` is the
/// anchor's rendered position (e.g. `"#[refines] at src/x.rs:3 (Foo::bar)"`);
/// `authoring_hint` appends the refinement/proof arms' "Either author the model…"
/// tail to the Ob.4 message; `noun`/`known` fix the Ob.1 wording ("action"/
/// "actions" vs "id"/"definitions"). `target == None` skips Ob.1 (a waiver with
/// no action). Message text is byte-identical to the former per-arm formats.
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

/// Enforce the four bidirectional obligations of TRUST_NATIVE_TLA §2.2 over the
/// given `SpecModule`s, the collected `refinements`, `waivers`, and
/// `invariant_anchors`. This is the runnable `trust-spec-link` (Phase 0).
///
/// Obligations enforced:
///   1. **Action exists** — every `refines`/`spec_invariant` action (and every
///      waiver action, when present) names a real definition in its module.
///   3. **Coverage** — for every machine that has ≥1 refinement (an *active*
///      machine), every model `Action` is bound-or-waived, i.e. `ratio == 1.0`.
///   4. **Machine exists** — every `machine` named by any anchor resolves to a
///      registered `SpecModule` (embedded or external). Catches a dangling machine.
///
/// **Coverage scoping (deliberate).** Requiring `ratio == 1.0` for ALL registered
/// models would paint a sea of red the moment any model (e.g. the kernel-family
/// twins, or the ISOLATION external specs) lacks a `#[refines]` handler — which is
/// expected today (they are bound via Tier-1 conformance / Phase 2, not via the
/// terminal_modes-style per-method `#[refines]`). So the `== 1.0` requirement is
/// scoped to ACTIVELY-BOUND machines (≥1 refinement), and the ratios of the rest
/// are merely REPORTED (their `MachineCoverage.active == false`). This is exactly
/// the §2.2 obligation-3 intent: total coverage where binding is claimed.
///
/// Obligation 2 ("symbol resolves to a live DefId") is NOT enforced here: it needs
/// the `trust-ir` symbol resolution of Phase 3. The Phase-0 aterm-local gate proves
/// 1/3/4 (the linkage/coverage closure); behavioural alignment is the separate
/// Tier-1 conformance layer (already green for window_routing).
///
/// A RUNNABLE FRAGMENT of obligation 2 does now exist, one layer up rather than in
/// this fn: [`StepEvidence`] answers "was the annotated function entered by the STEP
/// that drives this action?" from the probes `#[refines]` injects. `check_closure`
/// stays pure set algebra over the collected records (it is called in contexts with no
/// conformance to run, and by fixtures with hand-built anchors), so the execution
/// requirement is asserted by the gate that owns the conformance — today the
/// `SelectionCustody` and `PressCustody` blocks of `aterm_gui`'s `spec_xref_closure`,
/// which is 2 of the ~20 machines carrying anchors. Without it, a green `ratio=1.000`
/// here is a claim about STRINGS: an anchor moved onto a body-is-`false` stub with no
/// callers kept this fn green. WITH it, for those two machines, an anchor on a fn that
/// action's own windows do not enter fails by name, and so does one on a fn that IS
/// entered there but also runs when a different action of the machine fires
/// ([`StepAudit::stray`]) — but the evidence is function-entry, not branch-entry, and
/// [`StepAudit::summary`] prints in numbers what that leaves undecided.
// Skip: the closure gate's set algebra + format (BTreeSet/Map keyed builds,
// absent std bodies). THE xref gate itself — verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub fn check_closure(modules: &[SpecModule]) -> ClosureReport {
    let mut violations = Vec::new();

    // Index modules by THREE namespaces (TRUST_VACUITY_GATE §2.4):
    //   * the ACTION set (`action_names`, now Next-only for external) — the
    //     refinement/proof/waiver Ob.1 resolves against this;
    //   * the INVARIANT-def set (`invariant_names`, the full top-level def set for
    //     external) — the `#[spec_invariant]` id arm resolves against this ONLY;
    //   * the coverage-action set (`coverage_actions`) — the Ob.3 coverage check.
    // For embedded models all three coincide; for external `.tla` the action and
    // coverage sets are the `Next` disjuncts while the invariant set is the full def
    // set (so an invariant naming `TypeOK` resolves but a `#[refines]` naming it does
    // NOT — the alignment with Trust's already-strict artifact).
    // (machine name as declared, action set, invariant-def set, coverage-action set)
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
        // An invariant anchor's `id` should name a top-level DEFINITION in the
        // module (invariants legitimately name non-`Next` defs like `TypeOK`),
        // so it resolves against the WIDER `invariant_names()` set — NOT the
        // Next-only action set the refinement/proof arms use (§2.4).
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

    // ---- Obligation 4 + 1 for PROOF anchors (TRUST_NATIVE_TLA §4, Phase 4) ----
    // A `proof_anchor!`'d kani harness joins the SAME (machine, action) namespace as a
    // refinement, so it must satisfy the SAME structural obligations: its `machine`
    // resolves to a registered SpecModule (Ob.4) and its `action` exists in that machine
    // (Ob.1). This is the teeth of the unified ledger — a proof_anchor naming a bogus
    // action fails the gate exactly like a #[refines] would. (A proof anchor does NOT
    // discharge the coverage obligation — kani is bounded-LOCAL, not the temporal binding
    // coverage demands — so it is intentionally NOT folded into `bound` below.)
    for p in proof_anchors() {
        check_anchor_obligations(
            &mut violations,
            resolve(p.machine),
            AnchorObligationSpec {
                site: &format!("proof_anchor! at {} (proof `{}`)", p.location, p.proof_name),
                machine: p.machine,
                target: Some(p.action),
                select: |idx| &idx.1,
                authoring_hint: true,
                noun: "action",
                known: "actions",
            },
        );
    }

    // ---- Obligation 3: coverage over active machines, report over the rest ----
    // Bind/waive sets keyed by the MODULE's declared name (canonical).
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
    // Coverage is computed over the real coverage-action set (4th tuple element), NOT
    // the full def set — so an external machine is "fully covered" once every `Next`
    // disjunct is bound-or-waived, without demanding a `#[refines]` for `Init`/`TypeOK`.
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
    use crate::derive::{cursor_model, ring_model};
    use crate::tla_check::TlaSpec;

    /// TRUST_VACUITY_GATE §2.4 LOCK (finding 4): an EXTERNAL module's `action_names()`
    /// (the refinement/proof/waiver Ob.1 set) is the `Next` disjuncts ONLY, while
    /// `invariant_names()` (the `#[spec_invariant]` id set) is the full top-level def
    /// set. So an anchor naming `TypeOK` fails Ob.1 but an invariant naming `TypeOK`
    /// resolves — aligning the in-Rust gate with Trust's already-strict artifact (L3).
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

    // A real proof_anchor! invocation at module level (the kani-half of the ledger).
    // Submitted into the inventory slice for this crate's own unit-test binary, so the
    // collection / resolution / ledger path is exercised in aterm-spec itself (NOT inside
    // any #[cfg(kani)] block — that is the whole point of the decoupling).
    crate::proof_anchor!(
        machine = "Ring",
        action = "Push",
        proof = "aterm_spec_self_test_ring_push"
    );
    crate::proof_anchor!(
        machine = "Cursor",
        action = "Grow",
        proof = "aterm_spec_self_test_cursor_grow"
    );

    #[test]
    fn proof_anchor_macro_submits_a_collectable_kani_record() {
        let mine: Vec<_> = proof_anchors()
            .filter(|p| p.proof_name.starts_with("aterm_spec_self_test_"))
            .collect();
        assert_eq!(
            mine.len(),
            2,
            "both self-test proof anchors should be collected"
        );
        assert!(mine.iter().all(|p| p.kind == ProofKind::Kani));
        let ring = mine
            .iter()
            .find(|p| p.machine == "Ring")
            .expect("ring anchor");
        assert_eq!(ring.action, "Push");
        assert!(
            ring.location.contains("xref.rs"),
            "location is file:line: {}",
            ring.location
        );
    }

    #[test]
    fn proof_anchor_machine_action_resolves_under_the_closure() {
        // The proof anchors above name (Ring, Push) and (Cursor, Grow) — both REAL model
        // actions, so check_closure must NOT flag them (Ob.1/Ob.4 satisfied). We isolate
        // the proof-anchor obligations by checking no violation mentions a self-test proof.
        let modules = vec![
            SpecModule::Embedded(ring_model()),
            SpecModule::Embedded(cursor_model()),
        ];
        let report = check_closure(&modules);
        for v in &report.violations {
            assert!(
                !v.message.contains("aterm_spec_self_test_"),
                "a VALID self-test proof anchor was wrongly flagged: [Ob.{}] {}",
                v.obligation,
                v.message
            );
        }
    }

    // ── The per-step audit's DERIVATIONS, on synthetic ledgers ──────────────
    //
    // These exist because the two real ledgers are produced by a gate in another
    // crate: the arithmetic the gate PRINTS has to be checkable without running it,
    // and the field these replace (`ubiquitous`, an intersection across every window)
    // shipped green for weeks while being structurally incapable of firing.

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

    /// THE SHAPE OF THE TWO REAL LEDGERS, in miniature: one function carrying two
    /// actions and entered on every batch (`batch`, the `post_process` stand-in), one
    /// function private to a third action (`evict`), and one unrelated anchored
    /// function the batch also enters (`chatty`, the `write_char` stand-in).
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
        // `audit_steps_of` takes `&'static` anchors because the inventory yields them;
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

        // The old field: entered in EVERY window. `batch` misses the `Evict` window and
        // `evict` misses the two batch windows, so the intersection across all three is
        // empty — which is what made "0 functions were entered in EVERY step window"
        // print on both real machines while `post_process` was entered on every batch.
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

        // The replacement says the true thing: `batch` is entered under BOTH of its
        // actions, so its entry cannot say which fired.
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
        // `Evict`'s window entered exactly one: that window PINS its anchor.
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

    /// THE WITHIN-WINDOW MOVE, both halves: the one `stray` catches and the one it
    /// cannot, so the printed claim can name the boundary instead of gesturing at it.
    #[test]
    fn a_move_inside_one_window_is_caught_exactly_when_the_new_site_is_wider() {
        // CAUGHT. `Push` moves onto `chatty`, which the `Push` window really does
        // enter — so `witnessed` is satisfied — but `chatty` is entered by the `Pop`
        // window too, and it is not anchored for `Pop`. Its entry pattern is wider than
        // the action it now claims.
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

        // NOT CAUGHT, and the summary must not pretend otherwise. `Push` moves onto
        // `private`, a function only the `Push` window enters. Its entry pattern is
        // exactly the action it claims, so nothing in a per-step entry ledger separates
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

        // And the residual the anchor table cannot see either: permuting two actions
        // that share ONE function changes nothing observable at all.
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

    #[test]
    fn verifier_ledger_marks_kani_for_proof_anchored_actions() {
        let modules = vec![
            SpecModule::Embedded(ring_model()),
            SpecModule::Embedded(cursor_model()),
        ];
        let ledger = verifier_ledger(&modules);
        // Ring::Push is proof-anchored (kani=✓) by the self-test above.
        let ring_push = ledger
            .iter()
            .find(|e| e.machine == "Ring" && e.action == "Push")
            .expect("Ring::Push row");
        assert!(
            ring_push.kani,
            "Ring::Push must be kani-discharged in the ledger"
        );
        assert!(ring_push.proofs.contains("aterm_spec_self_test_ring_push"));
        // Cursor::Deliver is NOT proof-anchored here (kani=–).
        let deliver = ledger
            .iter()
            .find(|e| e.machine == "Cursor" && e.action == "Deliver")
            .expect("Cursor::Deliver row");
        assert!(
            !deliver.kani,
            "Cursor::Deliver has no proof anchor — kani must be –"
        );
        // The render is the per-(machine,action) ledger line shape.
        assert!(
            ring_push.render().contains("kani=✓  Ring::Push"),
            "{}",
            ring_push.render()
        );
        assert!(
            deliver.render().contains("kani=–  Cursor::Deliver"),
            "{}",
            deliver.render()
        );
    }
}
