// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Derived TLA+: one source of truth for a state machine, two faithful backends.
//!
//! "Derived" here means the spec and the interpreter are generated from ONE
//! AUTHORED model. It does not mean derived from the code: the model is written
//! by a person (as a [`Model`] value or through `ty_model!`), so it can drift
//! from the code it describes, and Tier 1 below is what ties them. Deriving a
//! model from the code itself is Trust's job (its D-11(e); design only in the
//! Trust repo's `docs/design/2026-09-29-descriptor-protocols-as-compiler-errors.md`).
//!
//! A hand-written `.tla` spec can drift from the code and from the executable
//! semantics a test checks; this module is the alternative (aterm commits none): a
//! [`Model`] is a first-class Rust value describing a bounded state machine
//! (constants, variables, guarded actions, invariants) over a small
//! expression language [`Expr`]. From that ONE source we generate BOTH:
//!
//!   - [`Model::to_tla`] / [`Model::to_cfg`] — a complete, `ty`-checkable TLA+
//!     module + config (the embedded spec), and
//!   - [`Model::fire`] / [`Model::successors`] — the executable transition
//!     semantics (a real interpreter, using TLA+ primed semantics: every
//!     right-hand side is evaluated against the pre-state, then applied).
//!     `successors` enumerates the existential fan-out of a nondeterministic
//!     ([`Expr::InRange`]) action; `fire` applies its canonical representative.
//!
//! Because both backends consume the same `Expr` trees, a change to the model
//! changes the spec AND the executable semantics together — they cannot drift.
//! The generated spec is exhaustively checked by `ty` (Tier 0); the same model
//! drives conformance against real code (Tier 1, see
//! `aterm-buffer/tests/conformance_eventlog.rs`).
//!
//! The expression language is intentionally small (what the bounded-ring model
//! needs); it grows as real models demand. The translation is mechanical:
//! `<=` ⇒ `=<`, `if/else` ⇒ `IF/THEN/ELSE`, `&&`/`||` ⇒ `/\`/`\/`.
//!
//! # The workflow, and what each tier proves
//!
//! For a new or changed bounded state machine:
//!
//! 1. **Model** it with `ty_model!` (operators `+ - > <= == && ||`, `if`/`else`;
//!    name a single action `Step`/`Push`/`Emit`, never `Append`). Writing it
//!    registers it (`xref::model_registry()`; a hand-built `Model` takes
//!    `#[aterm_spec::model]`). Write it in its domain's `aterm-models-<domain>`
//!    crate. Give each `invariant` a `Buggy` branch that breaks it
//!    (`aterm-models/tests/non_vacuity.rs` sweeps every registered model); state
//!    the explored box as `bound`s, ranges over one variable each. A claim no
//!    mutant can break asserts nothing: give it a mutant, or delete it.
//!    A configuration the real code also runs under (an owner's cap, a
//!    handoff) is a `scenario`, swept beside the committed one.
//!    `spec-check` (`aterm_spec::check`) holds the registry to `ty`.
//! 2. **Tier 0**: registering it is the Tier-0 check — `aterm-models/tests/non_vacuity.rs`
//!    proves it in every configuration it declares and breaks each invariant
//!    with a mutant, and `spec-check models` walks it with `ty`. `Err(NotRun)`
//!    (a function-valued model outside a checker run) is never a pass:
//!    `spec-check models` holds those under `ty`.
//! 3. **Tier 1**: in the crate's `tests/conformance_*.rs`, drive the real code,
//!    project its state onto the model and check real transitions against it,
//!    with a negative control. Anchor the real fns with
//!    `#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(…))]`; the
//!    crate's build script mounts `crates/aterm-spec-anchor/anchor_rules.rs` and its root
//!    carries `#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]`,
//!    the cfg its anchors compile in (aterm-spec-anchor's docs say why).
//! 4. **Progress** (it lands, drains, answers): state a `derive::Liveness` with
//!    named fairness and register it with `#[aterm_spec::liveness(its_model)]`,
//!    which `aterm-models/tests/non_vacuity.rs` discharges (`verify::audit_liveness`); copy
//!    `native_update_apply_ladder_liveness`.
//!
//! The models live in the `aterm-models-<domain>` crates. Models to copy:
//! `ring_model`, `subscribe_model`, `cursor_model`
//! (`aterm-models-core/src/models_engine_corpus.rs`); `kernel_model`, `snapshot_model`,
//! `transact_model` (`aterm-models-session/src/models_engine_corpus.rs`); for a writer/reader pair,
//! `NativeUpdateFailedMarkSuppression` (`aterm-models-update/src/models_update.rs`), whose negative
//! control replays the bug it was written for.
//!
//! What each step's result means (every result says which tier it reached):
//!
//! - **Tier 0 — the model.** `ty check` (or the in-process interpreter) explores
//!   the generated spec exhaustively within its bounds. It proves a property *of
//!   the model*, nothing about the code. Non-vacuity comes from a `Buggy` branch
//!   per invariant that the checker must refute
//!   (`crates/aterm-models/tests/non_vacuity.rs`).
//! - **Tier 1 — conformance.** A crate's `tests/conformance_*.rs` drives the real
//!   code, projects each reachable state onto the model's variables, and checks
//!   every observed transition against the same model: through its own guards and
//!   invariants, or with `ty trace validate` against [`Model::transition_spec`],
//!   plus a negative control that must be rejected
//!   (`aterm-buffer/tests/conformance_eventlog.rs` is the first). It binds model
//!   and executable over the states the test reaches, not over all inputs. The
//!   projection is written by hand and is the soundness pin: a wrong one can make
//!   conformance pass vacuously, which is why the negative control is required.
//! - **Tier 2 — refinement for all inputs** ("every real transition is allowed by
//!   the model") is not built here. It is Trust's model-to-program refinement
//!   work.
//!
//! `ty_model!` (`aterm-spec-macros`) is sugar over this module: a macro-written
//! model produces the byte-identical spec of the equivalent hand-built [`Model`]
//! (`crates/aterm-spec/tests/macro_ty_model.rs` pins this). Known limits: `Expr` is
//! scalar (bounded integers and booleans), so function- or array-valued state
//! needs an `Expr` extension first; bounds are finite; and a single-action model
//! must not name its action `Append`, which clashes with a `ty` Sequences builtin.
//! (`ty trace validate` checks `Next` on every transition, not only the first.)

use std::collections::BTreeMap;

/// A value in the model's (bounded-integer / boolean) semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bool(bool),
}

impl Value {
    // `as_int`/`as_bool` are the interpreter's type-punning accessors. `as_int` is
    // only ever applied to the result of evaluating an INTEGER-typed `Expr`
    // (`Int`/`Var`/`ConstRef`/`Add`/`Sub`, and the integer bounds of `InRange`/
    // `Forall`/`Exists`); `as_bool` only to a BOOLEAN-typed `Expr` (comparisons,
    // `Or`/`And`/`If`-cond/`Iff`, action guards, invariant bodies). Every `Model`
    // in this crate is built by the well-typed constructors below, so the crossed
    // arm (`Bool` for `as_int`, `Int` for `as_bool`) is unreachable on every real
    // evaluation. It still PANICS: like `eval`'s unbound-identifier arm, a default
    // value would silently mask an ill-typed model, so the crossed arm fails
    // closed, reclassified into the T9 contract-panic ledger column (below).
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "expected Int, got Bool")
    )]
    pub(crate) fn as_int(self) -> i64 {
        match self {
            Value::Int(i) => i,
            // Unreachable for well-typed models (see above): fail closed.
            Value::Bool(_) => panic!("expected Int, got Bool — model type error"),
        }
    }
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "expected Bool, got Int")
    )]
    pub(crate) fn as_bool(self) -> bool {
        match self {
            Value::Bool(b) => b,
            // Unreachable for well-typed models (see above): fail closed.
            Value::Int(_) => panic!("expected Bool, got Int — model type error"),
        }
    }
}

/// A small expression language shared by the TLA+ generator and the interpreter.
/// `Var`/`ConstRef` both resolve from the evaluation environment; arithmetic and
/// comparison map 1:1 to TLA+.
#[derive(Debug, Clone)]
pub enum Expr {
    /// An integer literal.
    Int(i64),
    /// A state variable reference (resolves from the current state).
    Var(&'static str),
    /// A TLA+ CONSTANT reference (resolves from the model's constants).
    ConstRef(&'static str),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    /// `a > b` (boolean).
    Gt(Box<Expr>, Box<Expr>),
    /// `a <= b` (boolean) — emits TLA+ `=<`.
    Le(Box<Expr>, Box<Expr>),
    /// `a = b` (boolean, integer operands) — emits TLA+ `=`.
    Eq(Box<Expr>, Box<Expr>),
    /// `a \/ b` (boolean disjunction) — emits parenthesized TLA+ `(a \/ b)` so it
    /// composes correctly when used as an action guard (which is conjoined with
    /// the update conjuncts, and `/\` binds tighter than `\/`).
    Or(Box<Expr>, Box<Expr>),
    /// `a /\ b` (boolean conjunction) — emits TLA+ `a /\ b` (unparenthesized; `/\`
    /// is associative with the action-level conjunction it joins, so a guard built
    /// from `And` composes correctly without parens).
    And(Box<Expr>, Box<Expr>),
    /// `IF cond THEN a ELSE b` (cond boolean; arms integer).
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `a <=> b` (boolean iff) — emits parenthesized `(a <=> b)`.
    Iff(Box<Expr>, Box<Expr>),
    /// `\A n \in lo..hi : body` (universal quantifier; `body` boolean, may
    /// reference the bound index by name). Scalar bodies are interpreter-evaluable.
    Forall(&'static str, Box<Expr>, Box<Expr>, Box<Expr>),
    // ---- function-valued (TLA+ generation only; see eval note) ----
    /// `fn[index]` — access a function-valued (`[1..N -> BOOLEAN]`) state variable.
    FnAccess(&'static str, Box<Expr>),
    /// `[fn EXCEPT ![index] = value]` — a point update of a function variable.
    Except(&'static str, Box<Expr>, Box<Expr>),
    /// `[n \in lo..hi |-> body]` — a function comprehension (`body` may reference
    /// the bound index by name); used for whole-function updates.
    Comprehension(&'static str, Box<Expr>, Box<Expr>, Box<Expr>),
    /// `TRUE` / `FALSE`.
    Bool(bool),
    /// `a # b` (integer inequality).
    Neq(Box<Expr>, Box<Expr>),
    /// `lo..hi` — a bounded integer SET, the only NONDETERMINISTIC construct. As an
    /// action update RHS it renders `var' \in lo..hi` (not `var' = ...`): the action
    /// admits ANY value in `lo..=hi` for that variable. `ty` checks the full
    /// existential fan-out exhaustively at Tier-0; the executable twin enumerates it
    /// via [`Model::successors`]. It is set-valued, so it has no scalar [`Expr::eval`]
    /// — it is meaningful ONLY as a top-level update RHS. Used where the scalar
    /// projection cannot pin a single next value (e.g. closing the frontmost window
    /// re-points `frontmost` to *some* surviving allocated id, not a computable one).
    InRange(Box<Expr>, Box<Expr>),
}

/// Convenience constructors (keep model definitions terse).
pub fn int(i: i64) -> Expr {
    Expr::Int(i)
}
pub fn var(n: &'static str) -> Expr {
    Expr::Var(n)
}
pub fn cst(n: &'static str) -> Expr {
    Expr::ConstRef(n)
}
pub fn add(a: Expr, b: Expr) -> Expr {
    Expr::Add(Box::new(a), Box::new(b))
}
pub fn sub(a: Expr, b: Expr) -> Expr {
    Expr::Sub(Box::new(a), Box::new(b))
}
pub fn gt(a: Expr, b: Expr) -> Expr {
    Expr::Gt(Box::new(a), Box::new(b))
}
pub fn le(a: Expr, b: Expr) -> Expr {
    Expr::Le(Box::new(a), Box::new(b))
}
pub fn eq(a: Expr, b: Expr) -> Expr {
    Expr::Eq(Box::new(a), Box::new(b))
}
pub fn or_(a: Expr, b: Expr) -> Expr {
    Expr::Or(Box::new(a), Box::new(b))
}
pub fn and_(a: Expr, b: Expr) -> Expr {
    Expr::And(Box::new(a), Box::new(b))
}
pub fn if_(c: Expr, a: Expr, b: Expr) -> Expr {
    Expr::If(Box::new(c), Box::new(a), Box::new(b))
}
pub fn iff(a: Expr, b: Expr) -> Expr {
    Expr::Iff(Box::new(a), Box::new(b))
}
pub fn forall(idx: &'static str, lo: Expr, hi: Expr, body: Expr) -> Expr {
    Expr::Forall(idx, Box::new(lo), Box::new(hi), Box::new(body))
}
pub fn fn_access(f: &'static str, index: Expr) -> Expr {
    Expr::FnAccess(f, Box::new(index))
}
pub fn except(f: &'static str, index: Expr, value: Expr) -> Expr {
    Expr::Except(f, Box::new(index), Box::new(value))
}
pub fn comprehension(idx: &'static str, lo: Expr, hi: Expr, body: Expr) -> Expr {
    Expr::Comprehension(idx, Box::new(lo), Box::new(hi), Box::new(body))
}
pub fn bool_lit(b: bool) -> Expr {
    Expr::Bool(b)
}
pub fn neq(a: Expr, b: Expr) -> Expr {
    Expr::Neq(Box::new(a), Box::new(b))
}
/// `lo..hi` — a bounded integer set, the nondeterministic update RHS. See
/// [`Expr::InRange`]: as an action update it admits any `lo..=hi` for the variable.
pub fn in_range(lo: Expr, hi: Expr) -> Expr {
    Expr::InRange(Box::new(lo), Box::new(hi))
}

impl Expr {
    /// Evaluate against an environment (state variables + constants).
    // AUDITED CONTRACT PANIC (T9): all three panics below are intentional
    // fail-closed contracts, not reachable bugs. An unbound identifier is a
    // spec-linkage typo (same contract as `action_enabled`: a default value
    // would silently mask it), and the function-valued / set-valued arms are
    // TLA+-generation-only Exprs that a scalar model never contains (see the
    // arm comments). The declarations reclassify the refuted panic-freedom
    // obligations into the always-visible contract-panic gate column.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "unbound identifier `")
    )]
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "function-valued Expr is TLA+-generation only")
    )]
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "InRange (`lo..hi`) is set-valued")
    )]
    // Skip: the sealed prover (321aaeda7) runs out of the 120 s function budget
    // on the whole body: all 77 obligations TIMEOUT, at every load measured
    // (2026-10-18, 36 GiB host; the 128 GiB host proves the same 151 in the
    // crate). A lapsed budget lets the lane read any drop below aterm-spec's
    // floor as NOT-VERIFIED(clock) instead of a regression, so a permanent one
    // blinds the floor. That day's toolchain proved it (1aa2de8eb8, 2026-07-14):
    // take the skip off when a seal proves it within its budget. Spec-model
    // machinery, the tier `compiled::Node::eval` is skipped at.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn eval(&self, env: &BTreeMap<&'static str, i64>) -> Value {
        match self {
            Expr::Int(i) => Value::Int(*i),
            // Inline `match` (not an `unwrap_or_else` closure) so the intentional
            // model-type-error panic is attributed to `eval` itself rather than a
            // standalone always-panicking closure; same panic, same message.
            Expr::Var(n) | Expr::ConstRef(n) => Value::Int(match env.get(n) {
                Some(v) => *v,
                None => panic!("unbound identifier `{n}` in model evaluation"),
            }),
            // Wrapping: the interpreter must be TOTAL on every valid Expr (an
            // overflow is a modelling fact to observe, not an abort), as in
            // `Model::successors`; this keeps `eval` panic-free without a skip.
            Expr::Add(a, b) => Value::Int(a.eval(env).as_int().wrapping_add(b.eval(env).as_int())),
            Expr::Sub(a, b) => Value::Int(a.eval(env).as_int().wrapping_sub(b.eval(env).as_int())),
            Expr::Gt(a, b) => Value::Bool(a.eval(env).as_int() > b.eval(env).as_int()),
            Expr::Le(a, b) => Value::Bool(a.eval(env).as_int() <= b.eval(env).as_int()),
            Expr::Eq(a, b) => Value::Bool(a.eval(env).as_int() == b.eval(env).as_int()),
            Expr::Neq(a, b) => Value::Bool(a.eval(env).as_int() != b.eval(env).as_int()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Or(a, b) => Value::Bool(a.eval(env).as_bool() || b.eval(env).as_bool()),
            Expr::And(a, b) => Value::Bool(a.eval(env).as_bool() && b.eval(env).as_bool()),
            Expr::If(c, a, b) => {
                if c.eval(env).as_bool() {
                    a.eval(env)
                } else {
                    b.eval(env)
                }
            }
            Expr::Iff(a, b) => Value::Bool(a.eval(env).as_bool() == b.eval(env).as_bool()),
            Expr::Forall(idx, lo, hi, body) => {
                let (l, h) = (lo.eval(env).as_int(), hi.eval(env).as_int());
                let mut e = env.clone();
                for n in l..=h {
                    e.insert(idx, n);
                    if !body.eval(&e).as_bool() {
                        return Value::Bool(false);
                    }
                }
                Value::Bool(true)
            }
            // Function-valued exprs are TLA+-generation only: the faithful models
            // that use them are Tier-0 ty-checked, not run through this scalar
            // interpreter (whose env is integer-valued). A scalar model never
            // contains these, so these arms are unreachable in practice.
            Expr::FnAccess(..) | Expr::Except(..) | Expr::Comprehension(..) => {
                panic!(
                    "function-valued Expr is TLA+-generation only (Tier-0 ty-checked, not interpreter-evaluable)"
                )
            }
            // `lo..hi` is a SET, not a scalar — there is no single value to return.
            // The interpreter consumes it only as an update RHS, where
            // `Model::successors` matches it and enumerates `lo..=hi` directly
            // (evaluating its bounds), so this arm is never reached in practice.
            Expr::InRange(..) => panic!(
                "InRange (`lo..hi`) is set-valued; it is meaningful only as a nondeterministic \
                 update RHS — fire the action via `Model::successors`, not scalar `eval`"
            ),
        }
    }

    /// Render as a TLA+ expression.
    // Skip: recursive TLA+ rendering — String allocs + the `Extend`/format
    // absent-body class. Spec-model machinery, same tier as `eval`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn to_tla(&self) -> String {
        match self {
            Expr::Int(i) => i.to_string(),
            Expr::Var(n) | Expr::ConstRef(n) => (*n).to_string(),
            Expr::Add(a, b) => format!("{} + {}", a.to_tla(), b.to_tla()),
            // TLA+ `+`/`-` are LEFT-associative, so a bare `a - b` renders correctly
            // for a scalar or left-nested `b`, but a compound ADDITIVE right operand
            // regroups: `Sub(x, Add(y,z))` -> `x - y + z` = (x-y)+z, and
            // `Sub(x, Sub(y,z))` -> `x - y - z` = (x-y)-z — both DIVERGE from the
            // interpreter, which evaluates the actual tree `x-(y+z)` / `x-(y-z)`. That
            // would let the ty-checked spec encode different arithmetic than its
            // executable twin (the exact drift this module promises cannot happen). So
            // parenthesize the RHS of a subtraction when it is itself an Add/Sub. Only
            // this position needs it: `+`'s operands and `-`'s LEFT operand are all
            // associativity-safe, and comparisons bind looser.
            Expr::Sub(a, b) => {
                let rhs = b.to_tla();
                let rhs = if matches!(**b, Expr::Add(..) | Expr::Sub(..)) {
                    format!("({rhs})")
                } else {
                    rhs
                };
                format!("{} - {}", a.to_tla(), rhs)
            }
            Expr::Gt(a, b) => format!("{} > {}", a.to_tla(), b.to_tla()),
            Expr::Le(a, b) => format!("{} =< {}", a.to_tla(), b.to_tla()),
            Expr::Eq(a, b) => format!("{} = {}", a.to_tla(), b.to_tla()),
            Expr::Neq(a, b) => format!("{} # {}", a.to_tla(), b.to_tla()),
            Expr::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            // Parenthesized: `\/` binds looser than the `/\` it is conjoined with.
            Expr::Or(a, b) => format!("({} \\/ {})", a.to_tla(), b.to_tla()),
            Expr::And(a, b) => format!("{} /\\ {}", a.to_tla(), b.to_tla()),
            // Parenthesized: an `IF`'s `ELSE` extends as far right as possible, so an
            // IF-valued update that is NOT the last action conjunct would otherwise
            // swallow the following `/\ ...`.
            Expr::If(c, a, b) => {
                format!(
                    "(IF {} THEN {} ELSE {})",
                    c.to_tla(),
                    a.to_tla(),
                    b.to_tla()
                )
            }
            Expr::Iff(a, b) => format!("({} <=> {})", a.to_tla(), b.to_tla()),
            Expr::Forall(idx, lo, hi, body) => {
                format!(
                    "\\A {idx} \\in {}..{} : {}",
                    lo.to_tla(),
                    hi.to_tla(),
                    body.to_tla()
                )
            }
            Expr::FnAccess(f, index) => format!("{f}[{}]", index.to_tla()),
            Expr::Except(f, index, value) => {
                format!("[{f} EXCEPT ![{}] = {}]", index.to_tla(), value.to_tla())
            }
            Expr::Comprehension(idx, lo, hi, body) => {
                format!(
                    "[{idx} \\in {}..{} |-> {}]",
                    lo.to_tla(),
                    hi.to_tla(),
                    body.to_tla()
                )
            }
            // A bounded set `lo..hi`. The action renderer special-cases an InRange
            // update RHS to emit `var' \in lo..hi`; this standalone form is the set
            // itself, for completeness of rendering.
            Expr::InRange(lo, hi) => format!("{}..{}", lo.to_tla(), hi.to_tla()),
        }
    }
}

/// `var' = expr`: an action's update to one state variable.
#[derive(Debug, Clone)]
pub struct Update {
    pub var: &'static str,
    pub expr: Expr,
}

/// A guarded action (a disjunct of `Next`). Variables not updated stay UNCHANGED.
#[derive(Debug, Clone)]
pub struct Action {
    pub name: &'static str,
    pub guard: Option<Expr>,
    pub updates: Vec<Update>,
}

/// A named safety invariant.
#[derive(Debug, Clone)]
pub struct Invariant {
    pub name: &'static str,
    pub expr: Expr,
    /// What it states, which decides what the non-vacuity sweep asks of it.
    pub kind: InvariantKind,
}

/// What an invariant states. `ty_model!` spells each with its own keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantKind {
    /// `invariant`: a design claim, which some mutant of the model must break
    /// or it asserts nothing (`verify::sweep_faults`).
    Claim,
    /// `bound`: the box the checkers walk, which no mutant is expected to
    /// break. It must be a range over one variable ([`Model::malformed_bounds`]),
    /// so a design claim cannot pass as one.
    Bound,
}

/// A named LIVENESS obligation over a derived [`Model`]: `[]<>goal` — on every
/// fair behaviour the goal keeps being reached, so no fair run leaves it for
/// good — under the fairness it states.
///
/// Why it exists: an invariant says a bad state is never reached and a deadlock
/// check says no state is stuck, and a machine can pass both while it never does
/// the one thing it is for. A lane that retries forever has only legal states,
/// and each has a successor. That is a livelock, and only a property about
/// whole behaviours can name it.
///
/// Why a companion value rather than a [`Model`] field: a model is one
/// literal written in more than a hundred places, and a liveness obligation is
/// rare and always hand-stated (its fairness is an assumption someone must own).
/// `#[liveness(model_fn)]` on its constructor registers it with its model in
/// `xref::liveness_registry` (the model joins the model registry too), which is
/// how the workspace sweep finds it. [`crate::interp::nonprogress_under`]
/// discharges it in-process; [`Liveness::to_tla`] / [`Liveness::to_cfg_with`] emit the same
/// property for `ty`, which checks `PROPERTY` formulas under the spec's declared
/// `WF_vars` / `SF_vars` fairness.
#[derive(Debug, Clone)]
pub struct Liveness {
    /// The property's name; `ty` checks it as the operator of this name, and
    /// `<name>Spec` is the fair specification it is checked under.
    pub name: &'static str,
    /// The goal: a boolean expression over the model's variables and constants.
    pub goal: Expr,
    /// Weakly fair actions (`WF_vars`): taken once they stay enabled. What
    /// each one assumes is the author's to say; see [`crate::interp::Fairness`].
    pub weak: Vec<&'static str>,
    /// Strongly fair actions (`SF_vars`): taken once they keep coming enabled.
    pub strong: Vec<&'static str>,
    /// The `Buggy` mutants — actions dead at the committed config — each of
    /// which, added ALONE to the `Buggy = 1` baseline, must break the property.
    /// An obligation no mutant can break is a ghost: it would pass whatever the
    /// lane did (`verify::audit_liveness` refuses one).
    pub mutants: Vec<&'static str>,
}

impl Liveness {
    /// Whether the goal holds at `state`, under `m`'s constants.
    ///
    /// # Panics
    ///
    /// If the goal is not boolean-typed — a model type error, failed closed like
    /// every other ill-typed evaluation in this module.
    // Skip: BTreeMap env build + the deliberate model-type-error panic (the
    // `eval` contract class). Spec-model machinery.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn goal_holds(&self, m: &Model, state: &BTreeMap<&'static str, i64>) -> bool {
        match self.goal.eval(&m.eval_env(state)) {
            Value::Bool(holds) => holds,
            Value::Int(_) => panic!(
                "liveness goal `{}` of `{}` is an integer — model type error",
                self.name, m.name
            ),
        }
    }

    /// The fair specification's operator name.
    #[must_use]
    pub fn spec_name(&self) -> String {
        format!("{}Spec", self.name)
    }

    /// `m`'s TLA+ module with the fair specification and the property appended:
    /// `<name>Spec == Spec /\ WF_vars(A) /\ … /\ SF_vars(B) …` and
    /// `<name> == []<>(goal)`. `m` is the model to emit — the committed one, or a
    /// mutant-isolated copy — so the fairness names must be actions it declares.
    // Skip: String assembly over the model's rendering (the format/alloc class),
    // same tier as `Model::to_tla`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn to_tla(&self, m: &Model) -> String {
        let module = m.to_tla();
        let body = module.strip_suffix("====\n").unwrap_or(&module);
        let mut spec = vec!["Spec".to_string()];
        spec.extend(self.weak.iter().map(|a| format!("WF_vars({a})")));
        spec.extend(self.strong.iter().map(|a| format!("SF_vars({a})")));
        format!(
            "{body}{} == {}\n{} == []<>({})\n====\n",
            self.spec_name(),
            spec.join(" /\\ "),
            self.name,
            self.goal.to_tla()
        )
    }

    /// The `.cfg` that checks the property alone: constants (with `overrides`),
    /// the fair specification, the `PROPERTY`, and no invariant — so a failure
    /// can only be the property's, never an invariant a mutant also breaks.
    // Skip: format/alloc, same tier as `Model::to_cfg_with`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn to_cfg_with(&self, m: &Model, overrides: &[(&'static str, i64)]) -> String {
        let mut s = String::new();
        for (n, default) in &m.consts {
            let val = overrides
                .iter()
                .find(|(o, _)| o == n)
                .map_or(*default, |(_, v)| *v);
            s.push_str(&format!("CONSTANT {n} = {val}\n"));
        }
        s.push_str(&format!(
            "SPECIFICATION {}\nPROPERTY {}\nCHECK_DEADLOCK FALSE\n",
            self.spec_name(),
            self.name
        ));
        s
    }
}

/// A state variable with its `Init` value.
#[derive(Debug, Clone)]
pub struct StateVar {
    pub name: &'static str,
    pub init: i64,
}

/// A function-valued state variable `[1..range -> BOOLEAN]`, initialized all-FALSE.
/// Used for per-element models (e.g. a ring's live-set) that the scalar projection
/// cannot express. Such models are Tier-0 `ty`-checked (TLA+ generation), not run
/// through the integer interpreter.
#[derive(Debug, Clone)]
pub struct FnVar {
    pub name: &'static str,
    /// Upper bound of the index domain (a CONSTANT name); the domain is `1..range`.
    pub range: &'static str,
}

/// A bounded state machine — the single source for both the TLA+ spec and the
/// executable semantics.
#[derive(Debug, Clone)]
pub struct Model {
    pub name: &'static str,
    pub consts: Vec<(&'static str, i64)>,
    pub vars: Vec<StateVar>,
    /// Function-valued variables (`[1..range -> BOOLEAN]`). Usually empty (scalar
    /// models); non-empty only for per-element Tier-0 models.
    pub fn_vars: Vec<FnVar>,
    pub actions: Vec<Action>,
    pub invariants: Vec<Invariant>,
}

impl Model {
    /// All state-variable names, scalar then function-valued (the order used in
    /// `VARIABLES`, `vars`, and `UNCHANGED`).
    // Skip: `Extend::extend` over a mapped iterator — the alloc + absent-body
    // class. Model-derivation machinery, same tier as `eval`/`successors`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn all_var_names(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.vars.iter().map(|x| x.name).collect();
        v.extend(self.fn_vars.iter().map(|f| f.name));
        v
    }

    /// The evaluation environment for `state`: the model's constants, then the
    /// state's variables on top (a state variable SHADOWS a same-named constant).
    ///
    /// Public because the env is a pure function of `(consts, pre-state)`, so it
    /// is IDENTICAL across every action and invariant evaluated at one state. The
    /// BFS in [`crate::interp`] builds it once per state and passes it to
    /// [`Model::successors_in`] / `Invariant::expr.eval`; per-(state, action)
    /// rebuilding cost a full-registry BMC sweep 2.54 s against 0.75 s (release).
    // Skip: the audited `BTreeMap` clone/entry alloc class of `init_state`.
    // Model-derivation machinery, same tier as `successors`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn eval_env(&self, state: &BTreeMap<&'static str, i64>) -> BTreeMap<&'static str, i64> {
        // The state's map cloned whole (a structural clone compares no keys),
        // then each constant the state does not bind. A state variable shadows a
        // same-named constant, and of two same-named constants the LAST wins
        // (hence the reverse walk with `or_insert`). Inserting the state into the
        // constants instead string-compares every variable, and that `memcmp`
        // led the profile of every by-name walk.
        let mut env = state.clone();
        for (k, v) in self.consts.iter().rev() {
            env.entry(*k).or_insert(*v);
        }
        env
    }

    /// Generate the complete TLA+ module text (the embedded, `ty`-checkable spec)
    /// with the model's concrete `Init` (variables start at their declared values).
    pub fn to_tla(&self) -> String {
        let const_names: Vec<String> = self.consts.iter().map(|(n, _)| (*n).to_string()).collect();
        let mut init_parts: Vec<String> = self
            .vars
            .iter()
            .map(|v| format!("{} = {}", v.name, v.init))
            .collect();
        // Function vars initialize to the all-FALSE function over `1..range`.
        for f in &self.fn_vars {
            init_parts.push(format!("{} = [n \\in 1..{} |-> FALSE]", f.name, f.range));
        }
        let init = init_parts.join(" /\\ ");
        self.render(&const_names, &init)
    }

    /// Generate the module with an OPEN `Init` (`Init == TRUE`): any state may be
    /// the start. Used for strict per-transition conformance against real code
    /// (Tier 1): a two-step trace `[prev, next]` starts wherever its step 0 says,
    /// so validating it strictly checks that `Next` admits the real
    /// `prev -> next` transition.
    ///
    /// Open rather than pinned to `prev`: the trace's step 0 already fixes the
    /// pre-state, and an open `Init` needs one cfg per `(model, overrides)`
    /// instead of one per `prev` (aterm-agent's footer suite needed 2,688).
    pub fn transition_spec(&self) -> String {
        let const_names: Vec<String> = self.consts.iter().map(|(n, _)| (*n).to_string()).collect();
        self.render(&const_names, "TRUE")
    }

    /// Shared renderer: emit the module given the CONSTANT names and the `Init`
    /// body. Actions, `Next`, `Spec`, and invariants are identical across the
    /// concrete and open-`Init` forms — one source for both.
    // Skip: the `filter(..).collect()` chain drives closure bodies that are
    // absent callees, plus String/Vec allocs. Spec-model TLA+ rendering,
    // same tier as `eval`/`to_tla`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn render(&self, const_names: &[String], init_line: &str) -> String {
        let vars = self.all_var_names();
        let mut s = String::new();
        s.push_str(&format!("---- MODULE {} ----\n", self.name));
        s.push_str("EXTENDS Naturals\n");
        if !const_names.is_empty() {
            s.push_str(&format!("CONSTANT {}\n", const_names.join(", ")));
        }
        s.push_str(&format!("VARIABLES {}\n", vars.join(", ")));
        s.push_str(&format!("vars == << {} >>\n", vars.join(", ")));
        s.push_str(&format!("Init == {init_line}\n"));

        // Actions
        for a in &self.actions {
            let mut conj: Vec<String> = Vec::new();
            if let Some(g) = &a.guard {
                conj.push(g.to_tla());
            }
            for u in &a.updates {
                match &u.expr {
                    // A nondeterministic update: `var' \in lo..hi` (membership), not
                    // `var' = ...` (equality). ty checks every value in the bounded
                    // range as a separate `Next` successor — the existential fan-out.
                    Expr::InRange(lo, hi) => {
                        conj.push(format!("{}' \\in {}..{}", u.var, lo.to_tla(), hi.to_tla()))
                    }
                    _ => conj.push(format!("{}' = {}", u.var, u.expr.to_tla())),
                }
            }
            // UNCHANGED for variables this action does not update.
            let updated: Vec<&str> = a.updates.iter().map(|u| u.var).collect();
            let unchanged: Vec<&str> = vars
                .iter()
                .copied()
                .filter(|v| !updated.contains(v))
                .collect();
            if !unchanged.is_empty() {
                conj.push(format!("UNCHANGED << {} >>", unchanged.join(", ")));
            }
            s.push_str(&format!("{} == {}\n", a.name, conj.join(" /\\ ")));
        }

        let action_names: Vec<&str> = self.actions.iter().map(|a| a.name).collect();
        s.push_str(&format!("Next == {}\n", action_names.join(" \\/ ")));
        s.push_str("Spec == Init /\\ [][Next]_vars\n");

        for inv in &self.invariants {
            s.push_str(&format!("{} == {}\n", inv.name, inv.expr.to_tla()));
        }
        s.push_str("====\n");
        s
    }

    /// Generate the `.cfg`: constant bindings, the specification, and every
    /// invariant. Bounded constants keep `ty check` exhaustive + terminating.
    pub fn to_cfg(&self) -> String {
        self.to_cfg_with(&[])
    }

    /// Like [`to_cfg`](Self::to_cfg) but with constant `overrides` — e.g. flip a
    /// `Buggy` flag to 1 to check that an invariant is non-trivial (the buggy
    /// variant must yield a counterexample), the in-spec analogue of the
    /// `Buggy`-constant convention used by the hand-written specs.
    pub fn to_cfg_with(&self, overrides: &[(&'static str, i64)]) -> String {
        let mut s = String::new();
        for (n, default) in &self.consts {
            let val = overrides
                .iter()
                .find(|(o, _)| o == n)
                .map(|(_, v)| *v)
                .unwrap_or(*default);
            s.push_str(&format!("CONSTANT {n} = {val}\n"));
        }
        s.push_str("SPECIFICATION Spec\n");
        for inv in &self.invariants {
            s.push_str(&format!("INVARIANT {}\n", inv.name));
        }
        s.push_str("CHECK_DEADLOCK FALSE\n");
        s
    }

    /// Like [`to_cfg_with`](Self::to_cfg_with) but with `ty` DEADLOCK / LIVENESS
    /// detection ON (`CHECK_DEADLOCK TRUE`) — the liveness twin of the safety
    /// configs, for request/reply "wedge" models (e.g. the forward handshake).
    ///
    /// Safety models check reachable bad STATES, but a blocking-call deadlock (the
    /// `drain_buffered` `fill_buf` hang — server parked reading more input while
    /// the client is parked awaiting the reply) is a two-party all-blocked wedge
    /// that only `CHECK_DEADLOCK` catches. The default `to_cfg`/`to_cfg_with` keep
    /// `CHECK_DEADLOCK FALSE`, since most models legitimately stop at bounded
    /// `MaxSeq`/`MaxN` terminals.
    ///
    /// DISCIPLINE (empirically enforced by `ty`): a model checked this way MUST
    /// give every legitimate work-complete terminal an explicit guarded
    /// zero-update `Done` self-loop, because TLA+ `[Next]_vars` stuttering does
    /// NOT count as an enabled step for deadlock purposes — without it `ty`
    /// flags the clean terminal as a deadlock. With it, the only reported
    /// deadlock is a genuine all-parties-parked wedge.
    #[must_use]
    pub fn to_cfg_deadlock_with(&self, overrides: &[(&'static str, i64)]) -> String {
        self.to_cfg_with(overrides)
            .replace("CHECK_DEADLOCK FALSE\n", "CHECK_DEADLOCK TRUE\n")
    }

    /// Config for [`transition_spec`](Self::transition_spec): binds each model
    /// constant (with optional `overrides`, e.g. the real ring capacity instead of
    /// the small exhaustive-check bound). Used for per-transition conformance —
    /// `ty trace validate --spec` then strictly checks the real `prev -> next`
    /// step against the derived `Next`. It names no state: the spec's `Init` is
    /// open, so one cfg serves every transition of a `(model, overrides)`.
    // Skip: `Extend`/format (absent std bodies). Spec-model cfg emission, same
    // tier as `to_tla`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn transition_cfg(&self, overrides: &[(&'static str, i64)]) -> String {
        let mut s = String::new();
        for (n, default) in &self.consts {
            let val = overrides
                .iter()
                .find(|(o, _)| o == n)
                .map(|(_, v)| *v)
                .unwrap_or(*default);
            s.push_str(&format!("CONSTANT {n} = {val}\n"));
        }
        s.push_str("SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n");
        s
    }

    /// The initial concrete state (variable -> value).
    // Skip: `collect::<BTreeMap>` bottoms out at tree-node allocation (abort,
    // not panic, on OOM — but the toolchain cannot yet see through the
    // generic collect) and `&'static str` Ord (total). Audited-alloc class;
    // verify-only. Re-measured 2026-09-27 on seal 321aaeda7 with this skip
    // removed: INCONCLUSIVE (its 1 obligation unknown). Needed from $HOME/trust,
    // in a promoted seal: totality for generic `collect` (TRUST_100PCT_PLAN
    // T3); then drop this.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn init_state(&self) -> BTreeMap<&'static str, i64> {
        self.vars.iter().map(|v| (v.name, v.init)).collect()
    }

    /// The `(machine, action)` anchor targets this model declares — the spec→source
    /// half of the cross-reference (TRUST_NATIVE_TLA §2.1). The closure gate
    /// (`crate::xref::check_closure`) resolves each `#[refines(machine, action)]`
    /// against this set: obligation 1 (the action exists in the model) and the
    /// coverage obligation (every action is bound-or-waived). `machine` is the
    /// model's own `name` for every action.
    pub fn anchors(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.actions.iter().map(move |a| (self.name, a.name))
    }

    /// Every next-state reachable by firing `action` from `state` — the executable
    /// twin of the generated TLA+ action's existential fan-out. TLA+ primed
    /// semantics: every right-hand side is evaluated against the PRE-state, then
    /// applied atomically. A purely deterministic action yields exactly ONE
    /// successor; an action with a nondeterministic [`Expr::InRange`] update yields
    /// one successor per value in its bounded `lo..=hi` range (the cartesian product
    /// when several updates are nondeterministic). Returns an EMPTY vector if the
    /// guard is unsatisfied OR any range is empty (`lo > hi`) — exactly the states
    /// where the generated `var' \in {}` makes the TLA+ action disabled.
    // Skip: the residual row is the `vec![..]` macro's idiomatic allocation
    // panic (the lz4 vec!-wrapper precedent); the product arithmetic above is
    // clamped and proven. Model-derivation machinery, same tier as `eval`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn successors(
        &self,
        action: &str,
        state: &BTreeMap<&'static str, i64>,
    ) -> Vec<BTreeMap<&'static str, i64>> {
        // `let-else` (not an `unwrap_or_else` closure) so the intentional
        // unknown-action panic is attributed to `successors` itself rather than a
        // standalone always-panicking closure; same panic, same message.
        let Some(act) = self.actions.iter().find(|a| a.name == action) else {
            panic!("no action `{action}` in model `{}`", self.name)
        };
        let env = self.eval_env(state);
        self.successors_in(act, &env, state)
    }

    /// [`Model::successors`] with the action ALREADY resolved and the evaluation
    /// environment ALREADY built — the form the BFS drivers in [`crate::interp`]
    /// use, where both are loop-invariant across one popped state. `successors`
    /// is the by-name wrapper over this; the semantics are identical.
    ///
    /// `env` must be [`Model::eval_env`] of `state` (consts, then state on top);
    /// passing any other map changes what the guard and update expressions see.
    // Skip: same class as `successors`, whose body this is.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn successors_in(
        &self,
        act: &Action,
        env: &BTreeMap<&'static str, i64>,
        state: &BTreeMap<&'static str, i64>,
    ) -> Vec<BTreeMap<&'static str, i64>> {
        if act.guard.as_ref().is_some_and(|g| !g.eval(env).as_bool()) {
            return Vec::new();
        }
        // Evaluate each update against the PRE-state into a list of candidate values
        // (one for a deterministic `=`, many for a nondeterministic `\in lo..hi`).
        let choices: Vec<(&'static str, Vec<i64>)> = act
            .updates
            .iter()
            .map(|u| match &u.expr {
                Expr::InRange(lo, hi) => {
                    // Function-path `Deref::deref` (not autoderef): the coercion
                    // spelling INLINES std's Box-deref unsafe block into this
                    // frame, where the L0 gate refutes it for lacking a local
                    // SAFETY comment; the fn-path call stays an opaque callee
                    // (total via the std-wrapper deref sentinel). Same `eval`
                    // calls in the same order — behavior-identical.
                    let lo: &Expr = std::ops::Deref::deref(lo);
                    let hi: &Expr = std::ops::Deref::deref(hi);
                    let (l, h) = (lo.eval(env).as_int(), hi.eval(env).as_int());
                    // Bounded push loop (not `(l..=h).collect()`, whose
                    // `RangeInclusive` iterator internals the Trust L0 gate cannot
                    // bound): pushes exactly the values `l..=h` ascending — empty
                    // when `l > h`, and the pre-increment `v == h` break makes the
                    // `h == i64::MAX` endpoint wrap-free, exactly like the
                    // range's own exhaustion check. Behavior-identical.
                    let mut vals = Vec::new();
                    let mut v = l;
                    while v <= h {
                        vals.push(v);
                        if v == h {
                            break;
                        }
                        // Wrapping: unreachable at `i64::MAX` (the `v == h` break
                        // above fired first, since `v <= h` held), so `v < h` here
                        // and the increment can never wrap — a no-op discharge of
                        // the overflow obligation (Trust L0).
                        v = v.wrapping_add(1);
                    }
                    (u.var, vals)
                }
                e => (u.var, vec![e.eval(env).as_int()]),
            })
            .collect();
        // Cartesian product over the per-update choice lists. Starts from the
        // unchanged pre-state (so variables no action touches stay put), then
        // branches each successor over every choice — ascending, so the FIRST
        // successor takes every range's lower bound (the canonical representative
        // `fire` selects). An empty choice list collapses the product to none.
        let mut acc: Vec<BTreeMap<&'static str, i64>> = vec![state.clone()];
        for (var, vals) in choices {
            // Deterministic update (`var' = expr`, the overwhelming majority):
            // exactly one candidate, so write it into every accumulated state IN
            // PLACE instead of cloning the accumulator (-20% on a full-registry
            // BMC sweep). Accumulator order is preserved, so the successor `fire`
            // reads as the canonical representative is unchanged.
            if let [only] = vals[..] {
                for s in &mut acc {
                    s.insert(var, only);
                }
                continue;
            }
            // Capacity is a pure pre-size HINT: saturating_mul kills the Mul
            // overflow obligation and the clamp bounds the up-front
            // allocation below the L0 unbounded-allocation gate; `push` grows
            // past the hint as needed, so contents are identical for every
            // input (successor counts are model-bounded in practice).
            let cap = acc.len().saturating_mul(vals.len()).min(4096);
            let mut next = Vec::with_capacity(cap);
            for base in &acc {
                for &v in &vals {
                    let mut s = base.clone();
                    s.insert(var, v);
                    next.push(s);
                }
            }
            acc = next;
        }
        acc
    }

    /// Fire a named action against `state`, applying the CANONICAL successor (TLA+
    /// primed semantics). Returns `false` without mutating if the action is disabled
    /// (guard unsatisfied or an empty nondeterministic range). For a deterministic
    /// action this is the unique successor; for a nondeterministic one it is the
    /// lower-bound representative — drive [`Model::successors`] directly to explore
    /// the full fan-out. This is the executable twin of the generated TLA+ action.
    pub fn fire(&self, action: &str, state: &mut BTreeMap<&'static str, i64>) -> bool {
        match self.successors(action, state).into_iter().next() {
            Some(next) => {
                *state = next;
                true
            }
            None => false,
        }
    }

    /// Whether `action`'s guard is satisfied in `state` (no guard ⇒ always
    /// enabled). This lets real code be checked against the model's ACTUAL guard
    /// expression — e.g. a conformance test can assert the real subscriber gaps
    /// exactly when `PollGap` is enabled — rather than re-stating the predicate.
    // AUDITED CONTRACT PANIC (T9): an unknown action name is a spec-linkage
    // typo; failing closed here is the documented contract (a default bool
    // would silently mask it).
    #[cfg_attr(trust_verify, trust::contract_panic(message_contains = "no action `"))]
    // Skip: BTreeMap keyed reads + the annotated model-type-error contract
    // panics it drives (same tier as `check_invariant`/`eval`).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn action_enabled(&self, action: &str, state: &BTreeMap<&'static str, i64>) -> bool {
        // `let-else` (not an `unwrap_or_else` closure): see `successors`.
        let Some(act) = self.actions.iter().find(|a| a.name == action) else {
            panic!("no action `{action}` in model `{}`", self.name)
        };
        let env = self.eval_env(state);
        act.guard
            .as_ref()
            .map(|g| g.eval(&env).as_bool())
            .unwrap_or(true)
    }

    /// Evaluate a named invariant against a concrete state.
    // AUDITED CONTRACT PANIC (T9): same fail-closed contract as
    // `action_enabled`, for invariant names.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "no invariant `")
    )]
    // Skip: BTreeMap keyed reads + the deliberate model-type-error contract
    // panics (annotated on `eval`, which this drives). Spec-model machinery.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_invariant(&self, name: &str, state: &BTreeMap<&'static str, i64>) -> bool {
        // `let-else` (not an `unwrap_or_else` closure): see `successors`.
        let Some(inv) = self.invariants.iter().find(|i| i.name == name) else {
            panic!("no invariant `{name}`")
        };
        let env = self.eval_env(state);
        self.check_invariant_in(inv, &env)
    }

    /// [`Model::check_invariant`] with the invariant ALREADY resolved and the
    /// evaluation environment ALREADY built — the [`Model::successors_in`] twin,
    /// for the BFS in [`crate::interp`] where both are loop-invariant across one
    /// popped state. `check_invariant` is the by-name wrapper over this.
    ///
    /// `env` must be [`Model::eval_env`] of the state under test.
    // Skip: same class as `check_invariant`, whose body this is (the deliberate
    // model-type-error contract panics are annotated on `eval`, which it drives).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_invariant_in(&self, inv: &Invariant, env: &BTreeMap<&'static str, i64>) -> bool {
        inv.expr.eval(env).as_bool()
    }

    /// The `Buggy` values the model's mutants are written for: 1 for a model
    /// that declares the dial, and every other literal it is compared with
    /// (`Buggy == 3`). Each dial is a defect the model was written to catch.
    // Skip: a recursive walk into a BTreeSet. Spec tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn mutant_dials(&self) -> Vec<i64> {
        fn walk(e: &Expr, out: &mut std::collections::BTreeSet<i64>) {
            match e {
                Expr::Eq(a, b) | Expr::Neq(a, b) => match (&**a, &**b) {
                    (Expr::ConstRef("Buggy"), Expr::Int(k))
                    | (Expr::Int(k), Expr::ConstRef("Buggy")) => {
                        out.insert(*k);
                    }
                    _ => {
                        walk(a, out);
                        walk(b, out);
                    }
                },
                Expr::ConstRef("Buggy") => {
                    out.insert(1);
                }
                Expr::Int(_) | Expr::Var(_) | Expr::ConstRef(_) | Expr::Bool(_) => {}
                Expr::Add(a, b)
                | Expr::Sub(a, b)
                | Expr::Gt(a, b)
                | Expr::Le(a, b)
                | Expr::Or(a, b)
                | Expr::And(a, b)
                | Expr::Iff(a, b)
                | Expr::InRange(a, b)
                | Expr::Except(_, a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Expr::If(a, b, c) | Expr::Forall(_, a, b, c) | Expr::Comprehension(_, a, b, c) => {
                    walk(a, out);
                    walk(b, out);
                    walk(c, out);
                }
                Expr::FnAccess(_, a) => walk(a, out),
            }
        }
        let mut out = std::collections::BTreeSet::new();
        if self.consts.iter().any(|(name, _)| *name == "Buggy") {
            out.insert(1);
        }
        for action in &self.actions {
            if let Some(guard) = &action.guard {
                walk(guard, &mut out);
            }
            for update in &action.updates {
                walk(&update.expr, &mut out);
            }
        }
        for invariant in &self.invariants {
            walk(&invariant.expr, &mut out);
        }
        out.into_iter().filter(|dial| *dial > 0).collect()
    }

    /// Whether the model has function-valued state, which only `ty` can
    /// check: the in-process interpreter evaluates scalar models alone.
    #[must_use]
    pub fn is_function_valued(&self) -> bool {
        !self.fn_vars.is_empty()
    }

    /// Why each declared bound is not a range, one line per fault: a bound
    /// is a conjunction of atoms comparing ONE state variable (`<=`, `>`)
    /// with literals and constants, leaving each variable it bounds more than
    /// one value. A relation between variables, a pin (`x <= 0`) or a bound
    /// over the `Buggy` dial is a design claim and needs its own mutant.
    // Skip: BTreeMap/String building over the model's bounds. Spec tooling.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn malformed_bounds(&self) -> Vec<String> {
        self.invariants
            .iter()
            .filter(|inv| inv.kind == InvariantKind::Bound)
            .flat_map(|inv| {
                not_range_bounds(&inv.expr, &self.consts)
                    .into_iter()
                    .map(move |why| format!("{}::{}: {why}", self.name, inv.name))
            })
            .collect()
    }
}

/// The value of `e` under `consts`, or `None` when it mentions a state
/// variable. `Buggy` is not a constant here: it is the mutant dial, and a bound
/// over it (`x <= Buggy`) pins the variable at the committed config.
fn constant_value(e: &Expr, consts: &[(&str, i64)]) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::ConstRef(name) if *name != "Buggy" => consts
            .iter()
            .find_map(|&(constant, value)| (constant == *name).then_some(value)),
        Expr::Add(a, b) => Some(constant_value(a, consts)? + constant_value(b, consts)?),
        Expr::Sub(a, b) => Some(constant_value(a, consts)? - constant_value(b, consts)?),
        _ => None,
    }
}

/// A range atom: ONE state variable compared with a constant expression,
/// either way round, as `(variable, lowest allowed, highest allowed)`.
fn range_atom(
    e: &Expr,
    consts: &[(&str, i64)],
) -> Option<(&'static str, Option<i64>, Option<i64>)> {
    let k = |e: &Expr| constant_value(e, consts);
    match e {
        Expr::Le(a, b) => match (&**a, &**b) {
            (Expr::Var(x), bound) => Some((*x, None, Some(k(bound)?))),
            (bound, Expr::Var(x)) => Some((*x, Some(k(bound)?), None)),
            _ => None,
        },
        Expr::Gt(a, b) => match (&**a, &**b) {
            (Expr::Var(x), bound) => Some((*x, Some(k(bound)? + 1), None)),
            (bound, Expr::Var(x)) => Some((*x, None, Some(k(bound)? - 1))),
            _ => None,
        },
        _ => None,
    }
}

/// Why `e` is not a range bound: each conjunct that is not a range atom, and
/// each variable its atoms pin to one value or none. Model variables count up
/// from zero, so an upper bound of zero, or a lower bound meeting the upper,
/// says "this never moves": a claim about the design, not the extent of the
/// space.
// Skip: BTreeMap/String building. Spec tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn not_range_bounds(e: &Expr, consts: &[(&str, i64)]) -> Vec<String> {
    fn conjuncts<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
        match e {
            Expr::And(a, b) => {
                conjuncts(a, out);
                conjuncts(b, out);
            }
            atom => out.push(atom),
        }
    }
    let mut atoms = Vec::new();
    conjuncts(e, &mut atoms);
    let mut bad = Vec::new();
    let mut extent: BTreeMap<&str, (i64, Option<i64>)> = BTreeMap::new();
    for atom in atoms {
        let Some((x, low, high)) = range_atom(atom, consts) else {
            bad.push(format!("`{}` is not a range atom", atom.to_tla()));
            continue;
        };
        let (lo, hi) = extent.entry(x).or_insert((0, None));
        *lo = (*lo).max(low.unwrap_or(0));
        *hi = match (*hi, high) {
            (Some(h), Some(n)) => Some(h.min(n)),
            (h, n) => h.or(n),
        };
    }
    for (x, (lo, hi)) in extent {
        if let Some(hi) = hi
            && hi <= lo
        {
            bad.push(format!("`{x}` is pinned to {lo}..={hi}"));
        }
    }
    bad
}

/// Property-combinator generators: each returns a fully-formed, `Buggy`-gated
/// [`Model`] (prove@Buggy=0, counterexample@Buggy=1) for a recurring property
/// CLASS. A new property is a struct literal + a `#[model]` fn, not 50
/// lines of `Expr` constructors. Every name (model / action / var / invariant) is
/// threaded through, so the emitted TLA+ is whatever the author wants — six of the
/// introspection models (M1 dispatch, M2 relay, S1 registry, the forward-handshake
/// liveness twin, F1 info-flow, and publish ordering) are byte-identical instances
/// of these generators.
///
/// The classes (the recurring shapes of the hand-built models): lifecycle/no-leak,
/// gated-completeness, happens-before, teardown-clears, two-stage-leak (info-flow),
/// and liveness/no-wedge.
pub mod props {
    use super::*;

    /// LOOSEN the buggy case: `(Buggy = 1 \/ cond)` — Buggy admits MORE (ordering).
    fn or_buggy(cond: Expr) -> Expr {
        or_(eq(cst("Buggy"), int(1)), cond)
    }
    /// RESTRICT to `cond` when buggy; correct admits freely: `(Buggy = 0 \/ cond)`.
    fn or_correct(cond: Expr) -> Expr {
        or_(eq(cst("Buggy"), int(0)), cond)
    }

    // ---- CLASS 1: lifecycle / no-leak ----
    pub struct Lifecycle {
        pub name: &'static str,
        pub live: &'static str,
        pub reg: &'static str,
        pub max: &'static str,
        pub max_val: i64,
        pub acquire: &'static str,
        pub release: &'static str,
        pub inv: &'static str,
    }
    /// `acquire` bumps both counters (bounded by `max`); `release` decrements `live`
    /// always and `reg` UNLESS Buggy (forgot to deregister). Invariant `reg =< live`.
    ///
    /// The mutant forgets only from a sound table (`reg =< live`): one slip is all
    /// the invariant needs to catch, and a mutant free to repeat it would strand one
    /// more entry per spawn/close cycle, so the `Buggy = 1` space `live` bounds
    /// would not be finite and the interpreter could not walk it.
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn lifecycle_no_leak(p: Lifecycle) -> Model {
        Model {
            name: p.name,
            consts: vec![(p.max, p.max_val), ("Buggy", 0)],
            vars: vec![
                StateVar {
                    name: p.live,
                    init: 0,
                },
                StateVar {
                    name: p.reg,
                    init: 0,
                },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: p.acquire,
                    guard: Some(le(add(var(p.live), int(1)), cst(p.max))),
                    updates: vec![
                        Update {
                            var: p.live,
                            expr: add(var(p.live), int(1)),
                        },
                        Update {
                            var: p.reg,
                            expr: add(var(p.reg), int(1)),
                        },
                    ],
                },
                Action {
                    name: p.release,
                    guard: Some(gt(var(p.live), int(0))),
                    updates: vec![
                        Update {
                            var: p.live,
                            expr: sub(var(p.live), int(1)),
                        },
                        Update {
                            var: p.reg,
                            expr: if_(
                                and_(eq(cst("Buggy"), int(1)), le(var(p.reg), var(p.live))),
                                var(p.reg),
                                sub(var(p.reg), int(1)),
                            ),
                        },
                    ],
                },
            ],
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Claim,
                expr: le(var(p.reg), var(p.live)),
            }],
        }
    }

    // ---- CLASS 2: gated completeness ----
    pub struct Gated {
        pub name: &'static str,
        pub item: &'static str,
        pub decided: &'static str,
        pub domain_max: &'static str,
        pub domain_val: i64,
        pub fwd_hi: i64,
        pub drop: i64,
        pub good: i64,
        pub bad: i64,
        pub pick: &'static str,
        pub route: &'static str,
        pub inv: &'static str,
    }
    /// `pick` fans `item' \in 0..domain_max`; `route` sets `decided := good` iff
    /// `item =< fwd_hi` AND (correct OR `item # drop`), else `bad`. Buggy drops the
    /// `drop` element. Invariant: `decided=0 \/ item>fwd_hi \/ decided=good`.
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn gated_completeness(p: Gated) -> Model {
        Model {
            name: p.name,
            consts: vec![(p.domain_max, p.domain_val), ("Buggy", 0)],
            vars: vec![
                StateVar {
                    name: p.item,
                    init: 0,
                },
                StateVar {
                    name: p.decided,
                    init: 0,
                },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: p.pick,
                    guard: Some(eq(var(p.decided), int(0))),
                    updates: vec![Update {
                        var: p.item,
                        expr: in_range(int(0), cst(p.domain_max)),
                    }],
                },
                Action {
                    name: p.route,
                    guard: Some(eq(var(p.decided), int(0))),
                    updates: vec![Update {
                        var: p.decided,
                        expr: if_(
                            and_(
                                le(var(p.item), int(p.fwd_hi)),
                                or_correct(neq(var(p.item), int(p.drop))),
                            ),
                            int(p.good),
                            int(p.bad),
                        ),
                    }],
                },
            ],
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Claim,
                expr: or_(
                    eq(var(p.decided), int(0)),
                    or_(
                        gt(var(p.item), int(p.fwd_hi)),
                        eq(var(p.decided), int(p.good)),
                    ),
                ),
            }],
        }
    }

    // ---- CLASS 3: happens-before (latch-pair ordering) ----
    pub struct Ordering {
        pub name: &'static str,
        pub a: &'static str,
        pub a_act: &'static str,
        pub b: &'static str,
        pub b_act: &'static str,
        pub inv: &'static str,
    }
    /// `a_act` sets `a:=1`; `b_act` (guard `b=0 /\ (Buggy=1 \/ a=1)`) sets `b:=1`.
    /// Buggy lets `b` race ahead of `a`. Invariant: `b=0 \/ a=1`.
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn happens_before(p: Ordering) -> Model {
        Model {
            name: p.name,
            consts: vec![("Buggy", 0)],
            vars: vec![
                StateVar { name: p.a, init: 0 },
                StateVar { name: p.b, init: 0 },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: p.a_act,
                    guard: Some(eq(var(p.a), int(0))),
                    updates: vec![Update {
                        var: p.a,
                        expr: int(1),
                    }],
                },
                Action {
                    name: p.b_act,
                    guard: Some(and_(eq(var(p.b), int(0)), or_buggy(eq(var(p.a), int(1))))),
                    updates: vec![Update {
                        var: p.b,
                        expr: int(1),
                    }],
                },
            ],
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Claim,
                expr: or_(eq(var(p.b), int(0)), eq(var(p.a), int(1))),
            }],
        }
    }

    // ---- CLASS 4: teardown clears N flags ----
    pub struct Teardown {
        pub name: &'static str,
        pub flags: Vec<&'static str>,
        pub gate: &'static str,
        pub act: &'static str,
        pub inv: &'static str,
    }
    /// `act` (guard `gate=0`) drops every flag to 0 (Buggy leaves them 1) and sets
    /// `gate:=1`. Invariant: `gate=0 \/ AND(flag=0)`. Flags init 1.
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn teardown_clears(p: Teardown) -> Model {
        let mut vars: Vec<StateVar> = p
            .flags
            .iter()
            .map(|f| StateVar { name: f, init: 1 })
            .collect();
        vars.push(StateVar {
            name: p.gate,
            init: 0,
        });
        let mut updates: Vec<Update> = p
            .flags
            .iter()
            .map(|f| Update {
                var: f,
                expr: if_(eq(cst("Buggy"), int(1)), int(1), int(0)),
            })
            .collect();
        updates.push(Update {
            var: p.gate,
            expr: int(1),
        });
        // Conjoin `flag = 0` over every flag. `reduce` yields `None` only for an
        // empty flag list, whose conjunction is TRUE, so the total completion is
        // exact and carries no panic obligation (Trust L0), unlike `expect`.
        let closed = p
            .flags
            .iter()
            .map(|f| eq(var(f), int(0)))
            .reduce(and_)
            .unwrap_or_else(|| bool_lit(true));
        Model {
            name: p.name,
            consts: vec![("Buggy", 0)],
            vars,
            fn_vars: vec![],
            actions: vec![Action {
                name: p.act,
                guard: Some(eq(var(p.gate), int(0))),
                updates,
            }],
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Claim,
                expr: or_(eq(var(p.gate), int(0)), closed),
            }],
        }
    }

    // ---- CLASS 5: two-stage leak (info-flow) ----
    pub struct TwoStage {
        pub name: &'static str,
        pub stage: &'static str,
        pub stage_act: &'static str,
        pub stage_rhs: Expr,
        pub leak: &'static str,
        pub leak_act: &'static str,
        pub leak_guard: Expr,
        pub leak_rhs: Expr,
        pub inv: &'static str,
        pub inv_expr: Expr,
    }
    /// `stage_act` (guard `stage=0`) sets `stage := stage_rhs`; `leak_act` (guard
    /// caller-supplied) sets `leak := leak_rhs`. The RHS exprs let the leak fire on a
    /// stage VALUE (secrecy) or on `Buggy` directly.
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn two_stage_leak(p: TwoStage) -> Model {
        Model {
            name: p.name,
            consts: vec![("Buggy", 0)],
            vars: vec![
                StateVar {
                    name: p.stage,
                    init: 0,
                },
                StateVar {
                    name: p.leak,
                    init: 0,
                },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: p.stage_act,
                    guard: Some(eq(var(p.stage), int(0))),
                    updates: vec![Update {
                        var: p.stage,
                        expr: p.stage_rhs,
                    }],
                },
                Action {
                    name: p.leak_act,
                    guard: Some(p.leak_guard),
                    updates: vec![Update {
                        var: p.leak,
                        expr: p.leak_rhs,
                    }],
                },
            ],
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Claim,
                expr: p.inv_expr,
            }],
        }
    }

    // ---- CLASS 6: liveness / no-wedge ----
    pub struct Wedge {
        pub name: &'static str,
        pub buffered: &'static str,
        pub buffered_init: i64,
        pub relayed: &'static str,
        pub waiting: &'static str,
        pub relay: &'static str,
        pub recv: &'static str,
        pub done: &'static str,
        pub inv: &'static str,
    }
    /// Two-party request→relay→reply. `relay` guard `(Buggy=0 \/ relayed>0) /\
    /// buffered>0` (Buggy demands a fresh read before the first relay → wedge);
    /// `recv` serves the client; `done` is the MANDATORY guarded zero-update
    /// self-loop on the served terminal (else `ty` flags it as a false deadlock).
    /// Pair with [`Model::to_cfg_deadlock_with`].
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn no_wedge(p: Wedge) -> Model {
        Model {
            name: p.name,
            consts: vec![("Buggy", 0)],
            vars: vec![
                StateVar {
                    name: p.buffered,
                    init: p.buffered_init,
                },
                StateVar {
                    name: p.relayed,
                    init: 0,
                },
                StateVar {
                    name: p.waiting,
                    init: 1,
                },
            ],
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: p.relay,
                    guard: Some(and_(
                        or_correct(gt(var(p.relayed), int(0))),
                        gt(var(p.buffered), int(0)),
                    )),
                    updates: vec![
                        Update {
                            var: p.relayed,
                            expr: add(var(p.relayed), int(1)),
                        },
                        Update {
                            var: p.buffered,
                            expr: sub(var(p.buffered), int(1)),
                        },
                    ],
                },
                Action {
                    name: p.recv,
                    guard: Some(and_(gt(var(p.relayed), int(0)), eq(var(p.waiting), int(1)))),
                    updates: vec![Update {
                        var: p.waiting,
                        expr: int(0),
                    }],
                },
                Action {
                    name: p.done,
                    guard: Some(eq(var(p.waiting), int(0))),
                    updates: vec![],
                },
            ],
            // `waiting <= 1` bounds a flag; the design claim is the
            // deadlock-freedom obligation the generator exists for.
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Bound,
                expr: le(var(p.waiting), int(1)),
            }],
        }
    }

    // ---- CLASS 7: conjunctive authorization soundness ----
    pub struct ConjunctiveAuthz {
        pub name: &'static str,
        pub guards: Vec<&'static str>,
        pub decided: &'static str,
        pub pick: &'static str,
        pub decide: &'static str,
        pub drop: &'static str,
        pub inv: &'static str,
    }
    /// `pick` sets each guard nondeterministically in `{0,1}`; `decide` PERMITS (2)
    /// iff EVERY guard holds, else DENIES (1) — except Buggy IGNORES the `drop` guard.
    /// Which conjunct is dropped is the bug class: dst → confused-deputy, op → op
    /// confusion, nonce → replay, token → forgery. Invariant: a permit implies EVERY
    /// guard truly held (authorization SOUNDNESS — the `decide_edge` predicate).
    // Skip (T2 vcgen-budget lane): a PARAMETERIZED spec-model data
    // constructor — same class as the zero-arg `*_model()` builders.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn conjunctive_authz(p: ConjunctiveAuthz) -> Model {
        let mut vars: Vec<StateVar> = p
            .guards
            .iter()
            .map(|g| StateVar { name: g, init: 0 })
            .collect();
        vars.push(StateVar {
            name: p.decided,
            init: 0,
        });
        let pick_updates: Vec<Update> = p
            .guards
            .iter()
            .map(|g| Update {
                var: g,
                expr: in_range(int(0), int(1)),
            })
            .collect();
        // Permit condition: AND of all guards; the `drop` conjunct is WAIVED when Buggy.
        // The empty conjunction is TRUE — the same total completion (and rationale)
        // as `teardown_clears` above; every real caller passes non-empty guards.
        let permit_when = p
            .guards
            .iter()
            .map(|g| {
                if *g == p.drop {
                    or_buggy(eq(var(g), int(1)))
                } else {
                    eq(var(g), int(1))
                }
            })
            .reduce(and_)
            .unwrap_or_else(|| bool_lit(true));
        // Soundness: a permit must imply EVERY guard actually held (no Buggy waiver).
        let all_hold = p
            .guards
            .iter()
            .map(|g| eq(var(g), int(1)))
            .reduce(and_)
            .unwrap_or_else(|| bool_lit(true));
        Model {
            name: p.name,
            consts: vec![("Buggy", 0)],
            vars,
            fn_vars: vec![],
            actions: vec![
                Action {
                    name: p.pick,
                    guard: Some(eq(var(p.decided), int(0))),
                    updates: pick_updates,
                },
                Action {
                    name: p.decide,
                    guard: Some(eq(var(p.decided), int(0))),
                    updates: vec![Update {
                        var: p.decided,
                        expr: if_(permit_when, int(2), int(1)),
                    }],
                },
            ],
            invariants: vec![Invariant {
                name: p.inv,
                kind: InvariantKind::Claim,
                expr: or_(neq(var(p.decided), int(2)), all_hold),
            }],
        }
    }
}

/// Three-slot GPU frame ring (`slot0_in_flight..slot2_in_flight`, `cursor` in `0..2`).
///
/// * At `Buggy = 0`, staging the next frame waits only if the target ring slot
///   `cursor` is still in flight on the GPU, then marks `cursor` in flight and
///   advances `cursor = (cursor + 1) % 3`, proving `NoOverwriteInFlight`.
/// * At `Buggy = 1`, staging into `cursor` without awaiting that slot's prior
///   GPU completion overwrites an in-flight buffer.
#[must_use]
#[cfg(feature = "anchors")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn gpu_frame_ring_model() -> Model {
    crate::ty_model! {
        GpuFrameRing {
            const Buggy = 0;

            var slot0_in_flight = 0;
            var slot1_in_flight = 0;
            var slot2_in_flight = 0;
            var cursor = 0;
            var overwritten = 0;

            action StageFrame when (
                Buggy == 1
                || (cursor == 0 && slot0_in_flight == 0)
                || (cursor == 1 && slot1_in_flight == 0)
                || (cursor == 2 && slot2_in_flight == 0)
            ) {
                overwritten = if (cursor == 0 && slot0_in_flight == 1)
                    || (cursor == 1 && slot1_in_flight == 1)
                    || (cursor == 2 && slot2_in_flight == 1)
                {
                    1
                } else {
                    overwritten
                };
                slot0_in_flight = if cursor == 0 { 1 } else { slot0_in_flight };
                slot1_in_flight = if cursor == 1 { 1 } else { slot1_in_flight };
                slot2_in_flight = if cursor == 2 { 1 } else { slot2_in_flight };
                cursor = if cursor == 2 { 0 } else { cursor + 1 };
            }

            action GpuCompleteSlot0 when (slot0_in_flight == 1) {
                slot0_in_flight = 0;
            }

            action GpuCompleteSlot1 when (slot1_in_flight == 1) {
                slot1_in_flight = 0;
            }

            action GpuCompleteSlot2 when (slot2_in_flight == 1) {
                slot2_in_flight = 0;
            }

            invariant NoOverwriteInFlight:
                overwritten == 0;
        }
    }
}

/// Single-hold predictive echo reconciliation across `ExtractSnapshot`,
/// `PtyEchoArrives`, `Reconcile`, and `PaintFrame`.
///
/// * At `Buggy = 0`, `Reconcile` reads from the already-extracted snapshot
///   (`snap_echoed`), proving `NoBlankEchoFrame` (`snap_echoed == 1 || ghost_visible == 1`
///   whenever a predicted char is in flight).
/// * At `Buggy = 1`, `Reconcile` re-locks the session mutex and reads `live_echoed`
///   after `PtyEchoArrives` interleaved between `ExtractSnapshot` (`snap_echoed == 0`)
///   and `Reconcile` (`live_echoed == 1`), retiring the ghost (`ghost_visible = 0`)
///   while `snap_echoed == 0` — catching the 1-frame blank-cell flicker.
#[must_use]
#[cfg(feature = "anchors")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn single_hold_prediction_reconcile_model() -> Model {
    crate::ty_model! {
        SingleHoldPredictionReconcile {
            const Buggy = 0;

            var phase = 0;
            var pred_in_flight = 1;
            var live_echoed = 0;
            var snap_echoed = 0;
            var ghost_visible = 1;
            var painted = 0;

            action ExtractSnapshot when (phase == 0) {
                snap_echoed = live_echoed;
                phase = 1;
            }

            action PtyEchoArrives when (live_echoed == 0) {
                live_echoed = 1;
            }

            action Reconcile when (phase == 1) {
                ghost_visible = if Buggy == 1 {
                    if live_echoed == 1 { 0 } else { 1 }
                } else {
                    if snap_echoed == 1 { 0 } else { 1 }
                };
                phase = 2;
            }

            action PaintFrame when (phase == 2) {
                painted = 1;
                phase = 3;
            }

            invariant NoBlankEchoFrame:
                pred_in_flight == 0 || snap_echoed == 1 || ghost_visible == 1;
        }
    }
}

/// Local compaction around the spliced `mid` run in `Rle::split_range_*`.
///
/// Models a 3-slot neighborhood `(left_val, mid_val, right_val)` surrounded by
/// compact outer neighbors `(outer_left_val != left_val, right_val != outer_right_val)`:
/// * At `Buggy = 0`, `compact_around(mid)` merges `mid` with `left` if equal and
///   with `right` if equal, establishing `NoAdjacentEqual` across the whole
///   neighborhood in $O(1)$ without scanning the outer runs.
/// * At `Buggy = 1`, omitting the right-neighbor check after a left merge leaves
///   adjacent runs with identical values when `left_val == mid_val == right_val`
///   (e.g. healing `[1, 2, 1]` by setting the middle run to `1`), caught by
///   `NoAdjacentEqual`.
#[must_use]
#[cfg(feature = "anchors")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rle_local_compact_model() -> Model {
    crate::ty_model! {
        RleLocalCompactModel {
            const Buggy = 0;

            var left_val = 1;
            var mid_val = 2;
            var right_val = 1;
            var merged_left = 0;
            var merged_right = 0;
            var compacted = 0;

            action SpliceSameAsNeighbors when (compacted == 0 && mid_val > 1) {
                mid_val = 1;
            }

            action SpliceDifferent when (compacted == 0 && mid_val == 2) {
                mid_val = 3;
            }

            action CompactAroundMid when (compacted == 0) {
                merged_left = if left_val == mid_val { 1 } else { 0 };
                merged_right = if Buggy == 1 && left_val == mid_val {
                    0
                } else {
                    if mid_val == right_val { 1 } else { 0 }
                };
                compacted = 1;
            }

            invariant NoAdjacentEqual:
                compacted == 0
                || ((left_val > mid_val || mid_val > left_val || merged_left == 1)
                    && (mid_val > right_val || right_val > mid_val || merged_right == 1));
        }
    }
}

/// Printable ASCII pair fast path in `aterm_grapheme::find_first_cluster_end`.
///
/// Models a 2-character prefix `(c0_ascii, c1_ascii, c1_extend)`:
/// * At `Buggy = 0`, the $O(1)$ fast path returns `cluster_end = 1` only when
///   BOTH `c0_ascii == 1` and `c1_ascii == 1` (`c1_extend == 0`), falling back
///   to the full UAX #29 scanner when `c1` is a non-ASCII combining mark
///   (`c1_extend == 1`), proving `CombiningMarkNeverSplit`.
/// * At `Buggy = 1`, returning `cluster_end = 1` after checking only `c0_ascii == 1`
///   splits a base ASCII character from its following combining mark (`a\u{0301}`),
///   caught by `CombiningMarkNeverSplit`.
#[must_use]
#[cfg(feature = "anchors")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ascii_grapheme_fast_path_model() -> Model {
    crate::ty_model! {
        AsciiGraphemeFastPath {
            const Buggy = 0;

            var c0_ascii = 1;
            var c1_ascii = 1;
            var c1_extend = 0;
            var decided = 0;
            var cluster_end = 0;

            action SecondIsCombiningMark when (decided == 0 && c1_ascii == 1) {
                c1_ascii = 0;
                c1_extend = 1;
            }

            action SecondIsAscii when (decided == 0 && c1_ascii == 0) {
                c1_ascii = 1;
                c1_extend = 0;
            }

            action FindClusterEnd when (decided == 0) {
                cluster_end = if Buggy == 1 {
                    if c0_ascii == 1 { 1 } else { 2 }
                } else {
                    if c0_ascii == 1 && c1_ascii == 1 {
                        1
                    } else {
                        if c1_extend == 1 { 2 } else { 1 }
                    }
                };
                decided = 1;
            }

            invariant CombiningMarkNeverSplit:
                decided == 0 || c1_extend == 0 || cluster_end == 2;
        }
    }
}

/// Formal model of PTY parse-thread end-of-batch yielding to a waiting UI
/// thread (`crates/aterm-gui/src/spawn.rs`).
///
/// At `Buggy = 0`, `ParseBatchFinish` yields at the end of every batch/chunk
/// whenever `ui_waiting == 1`, clearing the waiter before another batch can
/// grab `term_lock`. At `Buggy = 1`, skipping the end-of-batch yield when
/// `off == bytes.len()` allows `ParseBatchFinish` to complete without yielding
/// (`unyielded_with_waiter > 0`), violating `UiWaitBoundedToOneChunk`.
#[must_use]
#[cfg(feature = "anchors")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pty_batch_yield_fairness_model() -> Model {
    crate::ty_model! {
        PtyBatchYieldFairness {
            const MaxBatches = 3;
            const Buggy = 0;

            var batches_done = 0;
            var ui_waiting = 0;
            var unyielded_with_waiter = 0;

            action UiRequestLock when (ui_waiting == 0 && batches_done <= MaxBatches - 1) {
                ui_waiting = 1;
                unyielded_with_waiter = 0;
            }

            action UiAcquireOnYield when (ui_waiting == 1 && unyielded_with_waiter == 0) {
                ui_waiting = 0;
            }

            action ParseBatchFinish when (batches_done <= MaxBatches - 1) {
                batches_done = batches_done + 1;
                unyielded_with_waiter = if ui_waiting == 1 && Buggy == 1 {
                    unyielded_with_waiter + 1
                } else {
                    0
                };
                ui_waiting = if ui_waiting == 1 && Buggy == 0 {
                    0
                } else {
                    ui_waiting
                };
            }

            invariant UiWaitBoundedToOneChunk:
                unyielded_with_waiter == 0;
        }
    }
}

/// Formal model of `WarmTier` $O(1)$ cumulative-line offset eviction
/// (`crates/aterm-scrollback/src/tier.rs`).
///
/// At `Buggy = 0`, popping the oldest warm block advances `cumulative_base` by
/// `BlockLines` in $O(1)$, keeping `live_total == pushed_lines - evicted_lines`.
/// At `Buggy = 1`, omitting the `cumulative_base` advance on front eviction
/// leaves `live_total` stale, violating `TotalLinesMatchesLiveBlocks`.
#[must_use]
#[cfg(feature = "anchors")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn warm_tier_cumulative_offset_model() -> Model {
    crate::ty_model! {
        WarmTierCumulativeOffset {
            const MaxOps = 4;
            const BlockLines = 2;
            const Buggy = 0;

            var ops = 0;
            var pushed_lines = 0;
            var evicted_lines = 0;
            var cumulative_base = 0;
            var live_total = 0;

            action PushBlock when (ops <= MaxOps - 1) {
                ops = ops + 1;
                pushed_lines = pushed_lines + BlockLines;
                live_total = pushed_lines + BlockLines - cumulative_base;
            }

            action PopFrontBlock when (ops <= MaxOps - 1 && evicted_lines + BlockLines <= pushed_lines) {
                ops = ops + 1;
                evicted_lines = evicted_lines + BlockLines;
                cumulative_base = if Buggy == 1 {
                    cumulative_base
                } else {
                    cumulative_base + BlockLines
                };
                live_total = pushed_lines - (if Buggy == 1 {
                    cumulative_base
                } else {
                    cumulative_base + BlockLines
                });
            }

            invariant TotalLinesMatchesLiveBlocks:
                live_total == pushed_lines - evicted_lines;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_models::{evict_full_model, ring_model, subscribe_model};

    /// A pin and a bound over the `Buggy` dial are refused as bounds, while an
    /// ordinary one- or two-sided range passes.
    #[test]
    fn a_bound_must_be_a_range() {
        let var = || Box::new(Expr::Var("x"));
        let int = |n| Box::new(Expr::Int(n));
        let konst = |name| Box::new(Expr::ConstRef(name));
        let consts = [("Cap", 3), ("Buggy", 0)];
        let refused = |e: Expr| !not_range_bounds(&e, &consts).is_empty();

        assert!(refused(Expr::Le(var(), int(0))), "x <= 0 pins x");
        assert!(refused(Expr::Gt(int(1), var())), "1 > x pins x");
        assert!(
            refused(Expr::Le(var(), konst("Buggy"))),
            "Buggy is the dial"
        );
        assert!(
            refused(Expr::And(
                Box::new(Expr::Gt(var(), int(0))),
                Box::new(Expr::Le(var(), int(1))),
            )),
            "x > 0 && x <= 1 pins x to one"
        );
        assert!(
            refused(Expr::Le(var(), Box::new(Expr::Var("y")))),
            "a relation"
        );

        assert!(!refused(Expr::Le(var(), konst("Cap"))));
        assert!(!refused(Expr::And(
            Box::new(Expr::Gt(var(), int(0))),
            Box::new(Expr::Le(var(), Box::new(Expr::Sub(konst("Cap"), int(1))))),
        )));
    }

    #[test]
    fn sub_with_additive_rhs_parenthesizes_to_match_the_interpreter() {
        // Soundness regression: TLA+ +/- are left-associative, so a subtraction with a
        // compound additive RIGHT operand must be parenthesized or the rendered spec
        // encodes different arithmetic than Expr::eval. Assert to_tla() and eval()
        // agree for both shapes.
        let env = BTreeMap::new();

        // 5 - (2 - 1): eval = 4; bare `5 - 2 - 1` would be (5-2)-1 = 2.
        let e = sub(int(5), sub(int(2), int(1)));
        assert_eq!(e.to_tla(), "5 - (2 - 1)", "Sub RHS must be parenthesized");
        assert_eq!(e.eval(&env).as_int(), 4);

        // 5 - (2 + 1): eval = 2; bare `5 - 2 + 1` would be (5-2)+1 = 4.
        let e2 = sub(int(5), add(int(2), int(1)));
        assert_eq!(
            e2.to_tla(),
            "5 - (2 + 1)",
            "Sub RHS Add must be parenthesized"
        );
        assert_eq!(e2.eval(&env).as_int(), 2);

        // Left operand and non-additive RHS stay bare (no needless parens, so no
        // golden-string test regresses): (5 - 2) - 1 renders `5 - 2 - 1`.
        let e3 = sub(sub(int(5), int(2)), int(1));
        assert_eq!(e3.to_tla(), "5 - 2 - 1", "left-nested Sub needs no parens");
        assert_eq!(e3.eval(&env).as_int(), 2);
    }

    /// Pin the fail-closed policy of the crossed `Value` accessor arms: an
    /// ill-typed model Expr must PANIC (same contract class as eval's
    /// unbound-identifier arm), never quietly evaluate to a default.
    #[test]
    #[should_panic(expected = "expected Int, got Bool")]
    fn ill_typed_int_position_fails_closed() {
        // Add's operands must be integer-typed; a Bool there is a model bug.
        add(bool_lit(true), int(1)).eval(&BTreeMap::new());
    }

    /// The boolean-position twin of `ill_typed_int_position_fails_closed`.
    #[test]
    #[should_panic(expected = "expected Bool, got Int")]
    fn ill_typed_bool_position_fails_closed() {
        // And's operands must be boolean-typed; an Int there is a model bug.
        and_(int(1), bool_lit(true)).eval(&BTreeMap::new());
    }

    #[test]
    fn transition_spec_opens_init_but_shares_the_action() {
        let m = ring_model();
        let tla = m.transition_spec();
        assert!(tla.contains("CONSTANT MaxSeq, Cap, Buggy\n"), "{tla}");
        assert!(tla.contains("Init == TRUE\n"), "{tla}");
        // The action / Next / Spec are the SAME source as the concrete form.
        assert!(
            tla.contains("Push == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ lo' = (IF"),
            "{tla}"
        );
        let cfg = m.transition_cfg(&[("Cap", 65536), ("MaxSeq", 1_000_000)]);
        assert_eq!(
            cfg,
            "CONSTANT MaxSeq = 1000000\nCONSTANT Cap = 65536\nCONSTANT Buggy = 0\n\
             SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n"
        );
    }

    #[test]
    fn subscribe_emits_parenthesized_disjunction_guard_and_eq() {
        let tla = subscribe_model().to_tla();
        assert!(tla.contains("---- MODULE Subscribe ----"), "{tla}");
        // The disjunctive guard MUST be parenthesized, else `/\` captures it.
        assert!(
            tla.contains(
                "PollDeliver == (Buggy = 1 \\/ lo =< cursor + 1) /\\ cursor' = seq /\\ \
                 lost' = (IF lo > cursor + 1 THEN 1 ELSE lost) /\\ UNCHANGED << seq, lo >>"
            ),
            "{tla}"
        );
        assert!(
            tla.contains("Next == Grow \\/ PollGap \\/ PollDeliver"),
            "{tla}"
        );
        assert!(tla.contains("NoSilentLoss == lost = 0"), "{tla}");
    }

    #[test]
    fn evict_full_emits_function_valued_tla() {
        let tla = evict_full_model().to_tla();
        assert!(tla.contains("VARIABLES seq, lo, live"), "{tla}");
        assert!(
            tla.contains("live = [n \\in 1..MaxSeq |-> FALSE]"),
            "function init: {tla}"
        );
        assert!(
            tla.contains("[n \\in 1..MaxSeq |->"),
            "function comprehension: {tla}"
        );
        assert!(
            tla.contains("[live EXCEPT ![seq + 1] = TRUE]"),
            "EXCEPT update: {tla}"
        );
        assert!(tla.contains("live[n]"), "function access: {tla}");
        assert!(tla.contains("n # lo"), "inequality: {tla}");
        assert!(
            tla.contains(
                "EvictOldestContiguous == \\A n \\in 1..MaxSeq : (live[n] <=> lo =< n /\\ n =< seq)"
            ),
            "quantified iff invariant: {tla}"
        );
    }

    #[test]
    fn gpu_frame_ring_model_proves_and_catches_mutant() {
        let m = gpu_frame_ring_model();
        let states = crate::interp::prove_and_catch(&m);
        assert!(states > 1, "must explore multiple states: {states}");
        assert_eq!(
            crate::interp::fired_actions(&m).len(),
            m.actions.len(),
            "all actions must fire at Buggy=0"
        );
        crate::verify::prove_and_catch_scalar(&m, "GpuFrameRing");
    }

    #[test]
    fn single_hold_prediction_reconcile_model_proves_and_catches_mutant() {
        let m = single_hold_prediction_reconcile_model();
        let states = crate::interp::prove_and_catch(&m);
        assert!(states > 1, "must explore multiple states: {states}");
        assert_eq!(
            crate::interp::fired_actions(&m).len(),
            m.actions.len(),
            "all actions must fire at Buggy=0"
        );
        crate::verify::prove_and_catch_scalar(&m, "SingleHoldPredictionReconcile");
    }

    #[test]
    fn rle_local_compact_model_proves_and_catches_mutant() {
        let m = rle_local_compact_model();
        let states = crate::interp::prove_and_catch(&m);
        assert!(states > 1, "must explore multiple states: {states}");
        assert_eq!(
            crate::interp::fired_actions(&m).len(),
            m.actions.len(),
            "all actions must fire at Buggy=0"
        );
        crate::verify::prove_and_catch_scalar(&m, "RleLocalCompactModel");
    }

    #[test]
    fn ascii_grapheme_fast_path_model_proves_and_catches_mutant() {
        let m = ascii_grapheme_fast_path_model();
        let states = crate::interp::prove_and_catch(&m);
        assert!(states > 1, "must explore multiple states: {states}");
        assert_eq!(
            crate::interp::fired_actions(&m).len(),
            m.actions.len(),
            "all actions must fire at Buggy=0"
        );
        crate::verify::prove_and_catch_scalar(&m, "AsciiGraphemeFastPath");
    }

    #[test]
    fn pty_batch_yield_fairness_model_proves_and_catches_mutant() {
        let m = pty_batch_yield_fairness_model();
        let states = crate::interp::prove_and_catch(&m);
        assert!(states > 1, "must explore multiple states: {states}");
        assert_eq!(
            crate::interp::fired_actions(&m).len(),
            m.actions.len(),
            "all actions must fire at Buggy=0"
        );
        crate::verify::prove_and_catch_scalar(&m, "PtyBatchYieldFairness");
    }

    #[test]
    fn warm_tier_cumulative_offset_model_proves_and_catches_mutant() {
        let m = warm_tier_cumulative_offset_model();
        let states = crate::interp::prove_and_catch(&m);
        assert!(states > 1, "must explore multiple states: {states}");
        assert_eq!(
            crate::interp::fired_actions(&m).len(),
            m.actions.len(),
            "all actions must fire at Buggy=0"
        );
        crate::verify::prove_and_catch_scalar(&m, "WarmTierCumulativeOffset");
    }
}
