// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The compiler-collectable source↔spec cross-reference (TRUST_NATIVE_TLA, §2).

pub use aterm_spec_anchor::xref::{
    RefinementAnchor, Waiver, disarm_entered_anchors, entered_anchor_count, entered_anchor_ids,
    merge_entered_anchors, note_entered, refinements, reset_entered_anchors, take_entered_anchors,
    waivers, window_is_armed,
};

use std::collections::{BTreeMap, BTreeSet};

use crate::derive::Model;

/// What one conformance step entered: the action it claims to drive, and the anchor ids whose annotated fn ran inside that step's window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    /// The machine action this step drives, spelled as the anchors spell it.
    pub action: &'static str,
    /// Anchor ids entered between the window opening and closing.
    pub entered: BTreeSet<&'static str>,
}

/// A machine's per-step execution evidence, accumulated by its Tier-1 conformance (the only party that knows where one step ends and the next begins) and audited by the gate that ran it.
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
    /// # Panics
    /// If the window cannot be opened ([`reset_entered_anchors`]), the record is not observably empty once it is ([`entered_anchor_count`]), or the window was closed while `drive` ran ([`window_is_armed`]): each would let the audit pass on stale evidence.
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
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn audit(&self) -> StepAudit {
        audit_steps(self.machine, &self.steps)
    }
}

/// The execution-evidence verdict for one machine, crossing its linked anchors with a [`StepEvidence`] ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAudit {
    /// The machine, spelled as the caller spelled it.
    pub machine: String,
    /// How many of the machine's anchors are linked into this binary (probed or not).
    pub anchors: usize,
    /// The distinct actions those anchors cover.
    pub actions: BTreeSet<&'static str>,
    /// How many step windows the conformance recorded.
    pub steps: usize,
    /// Anchors the macro could not probe (`entry_id == ""`), by `rust_method`: a failure for a machine claiming execution evidence.
    pub uninstrumented: Vec<&'static str>,
    /// `entry_id`s claimed by more than one anchor, which would let one fn's entry discharge another's obligation.
    pub ambiguous: Vec<&'static str>,
    /// Anchored actions with no step in the ledger: the conformance never claimed to drive them, so nothing was checked about their anchors.
    pub undriven: Vec<&'static str>,
    /// Anchored actions that were driven and whose annotated function was entered in no step of their own: the failure list (an anchor on a stub, on a fn the action's step never reaches, or on an unrelated fn).
    pub unwitnessed: Vec<&'static str>,
    /// One entry per window that did not enter its own action's function, named by that action (so an action driven three times can appear twice).
    pub blind_steps: Vec<&'static str>,
    /// Step tags that match no anchored action of this machine — a typo in the conformance, checking nothing.
    pub unanchored_steps: Vec<&'static str>,
    /// Annotated fn → the actions of this machine anchored on it.
    pub by_function: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Actions sharing their annotated fn with a sibling action, which entry alone cannot tell apart.
    pub shared_site: BTreeSet<&'static str>,
    /// Action → the anchored functions every window driving it entered: what a window pins.
    pub interchangeable: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Anchored function of this machine → the actions whose windows entered it.
    pub entered_under: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Actions one of whose anchored functions was entered by another action's window, so entry says the function ran, not which action fired: the run-derived twin of [`Self::shared_site`].
    pub indiscriminate: BTreeSet<&'static str>,
    /// Anchored function of this machine → the actions whose windows entered it that it is not anchored for.
    pub stray: BTreeMap<&'static str, BTreeSet<&'static str>>,
    /// Per step window, in the order driven: the action it drives and the distinct anchored functions it entered.
    pub window_reach: Vec<(&'static str, BTreeSet<&'static str>)>,
    /// Anchor sites of a multi-site action that no window of that action ever entered.
    pub dark_sites: Vec<&'static str>,
    /// How many anchor sites belong to an action with more than one: the denominator of [`Self::dark_sites`], so `0` dark sites over `0` reads as "no action has a second site yet", not an all-clear.
    pub multi_site_anchors: usize,
    /// Steps whose window entered an anchor of their own action.
    pub witnessed_steps: usize,
}

impl StepAudit {
    /// Did every anchored action get driven and witnessed, with none unprobed, ambiguous, or tagged by a step that names no action?
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

    /// The widest interchangeability set: `(action, k)` for the action whose windows pin its anchor least — `k` already-anchored functions any of which would pass.
    #[must_use]
    pub fn widest_window(&self) -> (&'static str, usize) {
        self.interchangeable
            .iter()
            .map(|(a, f)| (*a, f.len()))
            .max_by_key(|(a, n)| (*n, core::cmp::Reverse(*a)))
            .unwrap_or(("<no window>", 0))
    }

    /// How many windows entered more than one anchored function — the windows inside which this check distinguishes nothing — and the widest of them, spelled out: the action it drives and every anchored function it entered.
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
fn fn_of(entry_id: &'static str) -> &'static str {
    entry_id.rsplit_once(" @ ").map_or(entry_id, |(_, f)| f)
}

/// Cross `machine`'s linked anchors with a per-step ledger. `machine` is resolved with
#[cfg_attr(trust_verify, trust::skip)]
fn audit_steps(machine: &str, steps: &[StepRecord]) -> StepAudit {
    let linked: Vec<&'static RefinementAnchor> = refinements()
        .filter(|r| machine_matches(r.machine, machine))
        .collect();
    audit_steps_of(machine, &linked, steps)
}

/// The pure half of [`audit_steps`]: set algebra over an explicit anchor list, so the
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_lines)]
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

    let blind_steps: Vec<&'static str> = steps
        .iter()
        .filter(|s| actions.contains(&s.action) && !witnessed_of(s))
        .map(|s| s.action)
        .collect();

    let fns_of_step: Vec<BTreeSet<&'static str>> = steps
        .iter()
        .map(|s| s.entered.iter().copied().map(fn_of).collect())
        .collect();

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

    let mut entered_under: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    for (s, fns) in steps.iter().zip(&fns_of_step) {
        if !actions.contains(&s.action) {
            continue;
        }
        for f in fns.iter().copied().filter(|f| by_function.contains_key(f)) {
            entered_under.entry(f).or_default().insert(s.action);
        }
    }

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

    let stray: BTreeMap<&'static str, BTreeSet<&'static str>> = entered_under
        .iter()
        .filter_map(|(f, under)| {
            let own = by_function.get(f)?;
            let extra: BTreeSet<&'static str> = under.difference(own).copied().collect();
            (!extra.is_empty()).then_some((*f, extra))
        })
        .collect();

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

/// One registered model.
#[derive(Debug, Clone, Copy)]
pub struct RegisteredModel {
    /// Builds the model.
    pub build: fn() -> Model,
    /// The configurations it declares beside the committed one (`scenario` in `ty_model!`): each a set of constant overrides the real code also runs under, swept like the committed one.
    pub scenarios: &'static [&'static [(&'static str, i64)]],
    /// Where it was written (`file:line`).
    pub location: &'static str,
}

/// One registered liveness obligation, submitted by `#[liveness(model)]` with the model it is stated over.
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

/// Every model linked into this binary, in name order.
#[cfg_attr(trust_verify, trust::skip)]
pub fn model_registry() -> Vec<Model> {
    registered().into_values().map(|(m, _, _)| m).collect()
}

/// Every registered model in every configuration it declares (the committed one first, then each `scenario`), in [`model_registry`]'s order.
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

/// Every liveness obligation linked into this binary, beside the registered model it is stated over, in (model, obligation) name order.
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

/// A registered model, the scenarios it declares, and where it was written.
type Registered = (
    Model,
    &'static [&'static [(&'static str, i64)]],
    &'static str,
);

/// Every registered model by name: each one a `ty_model!` or `#[model]` registers, and each one a
/// # Panics
///
/// When two different models share a name.
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

/// A registered spec module: an embedded `ty_model!`/`derive` [`Model`].
pub enum SpecModule {
    Embedded(Model),
}

impl SpecModule {
    /// The machine name this module declares.
    pub fn name(&self) -> &str {
        match self {
            SpecModule::Embedded(m) => m.name,
        }
    }

    /// The model's action names: what a refinement or waiver anchor resolves against (obligation 1) and what coverage counts (obligation 3).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn action_names(&self) -> BTreeSet<String> {
        let SpecModule::Embedded(m) = self;
        let mut names = BTreeSet::new();
        for (_, a) in m.anchors() {
            names.insert(a.to_string());
        }
        names
    }
}

/// Whether an anchor's `machine` resolves to a `SpecModule` named `module_name`: anchors are lower_snake (`"terminal_modes"`), models CamelCase (`"TerminalModes"`), so the match is case-insensitive with `_` stripped.
pub fn machine_matches(anchor_machine: &str, module_name: &str) -> bool {
    #[cfg_attr(trust_verify, trust::skip)]
    fn norm(s: &str) -> String {
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
    /// Which §2.2 obligation was violated: 1, 3 or 4.
    pub obligation: u8,
    pub message: String,
}

/// One module's coverage line for the report.
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
        let covered = self.bound.len().saturating_add(self.waived.len());
        covered as f64 / self.total_actions.max(1) as f64
    }
}

/// The outcome of [`check_closure`]: the violations (empty == green) and the per-machine coverage ledger (for reporting).
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

/// One module's action set, keyed by its declared machine name.
type ModuleActionIndex = (String, BTreeSet<String>);

struct AnchorObligationSpec<'a> {
    site: &'a str,
    machine: &'a str,
    target: Option<&'a str>,
    authoring_hint: bool,
    noun: &'a str,
    known: &'a str,
}

/// The Ob.4 ("machine exists") and Ob.1 ("action exists") arm for one anchor: push a
fn check_anchor_obligations(
    violations: &mut Vec<ClosureViolation>,
    resolved: Option<&ModuleActionIndex>,
    spec: AnchorObligationSpec<'_>,
) {
    let AnchorObligationSpec {
        site,
        machine,
        target,
        authoring_hint,
        noun,
        known,
    } = spec;
    match resolved {
        None => {
            let tail = if authoring_hint {
                " (no registered Model). Either author the model or fix the machine \
                 name."
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
        Some((_, set)) => {
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

/// Enforce the bidirectional obligations of TRUST_NATIVE_TLA §2.2 over `modules` and the collected refinements and waivers:
#[cfg_attr(trust_verify, trust::skip)]
pub fn check_closure(modules: &[SpecModule]) -> ClosureReport {
    let mut violations = Vec::new();

    let module_actions: Vec<ModuleActionIndex> = modules
        .iter()
        .map(|m| (m.name().to_string(), m.action_names()))
        .collect();

    let resolve = |anchor_machine: &str| -> Option<&ModuleActionIndex> {
        module_actions
            .iter()
            .find(|(name, _)| machine_matches(anchor_machine, name))
    };

    for r in refinements() {
        check_anchor_obligations(
            &mut violations,
            resolve(r.machine),
            AnchorObligationSpec {
                site: &format!("#[refines] at {} ({})", r.location, r.rust_method),
                machine: r.machine,
                target: Some(r.action),
                authoring_hint: true,
                noun: "action",
                known: "actions",
            },
        );
    }

    for w in waivers() {
        if w.machine.is_empty() {
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
                authoring_hint: false,
                noun: "action",
                known: "actions",
            },
        );
    }

    let mut bound: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut waived: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for r in refinements() {
        if let Some((name, _)) = resolve(r.machine) {
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
        if let Some((name, _)) = resolve(w.machine) {
            waived
                .entry(name.clone())
                .or_default()
                .insert(w.action.to_string());
        }
    }

    let mut coverage = Vec::new();
    for (name, actions) in &module_actions {
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

    /// A synthetic anchor. `entry_id` is spelled exactly as the `#[refines]` macro
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

    fn audit_of(anchors: &[RefinementAnchor], steps: &[StepRecord]) -> StepAudit {
        let linked: Vec<&'static RefinementAnchor> =
            anchors.iter().map(|a| &*Box::leak(Box::new(*a))).collect();
        audit_steps_of("Ring", &linked, steps)
    }

    /// The within-window move `stray` catches: an anchor moved onto a function whose entry
    #[test]
    fn a_move_inside_one_window_is_caught_when_the_new_site_is_wider() {
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
    #[test]
    fn a_workers_window_reaches_its_owner_only_by_take_and_merge() {
        assert!(reset_entered_anchors(), "the owner's window opens");
        let taken = std::thread::scope(|scope| {
            scope
                .spawn(|| {
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
