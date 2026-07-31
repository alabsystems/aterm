// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Derived TLA+: one source of truth for a state machine, two faithful backends.
//!
//! The specs in `aterm-spec-models/specs/` are hand-written `.tla` files — they
//! can drift from the code. This module is the *derivation half* of
//! `docs/RFC-ty-embed-derived-tla.md`: a [`Model`] is a first-class Rust value
//! describing a bounded state machine (constants, variables, guarded actions,
//! invariants) over a small expression language [`Expr`]. From that ONE source we
//! generate BOTH:
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
    fn as_int(self) -> i64 {
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
    fn as_bool(self) -> bool {
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
    // NO skip: `eval` VERIFIES. It is a recursive model-checker evaluator over a
    // `Box<Expr>` tree — historically the workspace's hardest function — and it now
    // discharges (aterm-spec gate CONDITIONAL-PASS, exit 0: 0 failed, 0 unknown) with
    // eval's only rows being the 3 INTENTIONAL model-type-error `panic!` arms
    // (annotated above), which land in the non-blocking contract-panic ledger column.
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
            // `wrapping_add`/`wrapping_sub`: a spec-model interpreter must be TOTAL on
            // every syntactically valid Expr (an overflow is a spec-arithmetic modelling
            // fact to observe, not a runtime abort), matching `Model::successors`, which
            // already uses `wrapping_add` for the same reason. This makes `eval`
            // panic-free arithmetic-wise, so the whole function discharges without a skip.
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

    /// Render as a TLA+ expression. `prime` marks state-variable references with a
    /// trailing `'` (used on action right-hand sides? no — RHS reads pre-state, so
    /// `prime` is false there; it exists for completeness of variable rendering).
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
            // associativity-safe, and comparisons bind looser, so no committed model's
            // rendered output changes (no golden-string test regresses).
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

    /// Constants as an evaluation environment base.
    // Skip: same audited `collect::<BTreeMap>` alloc class as `init_state`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn const_env(&self) -> BTreeMap<&'static str, i64> {
        self.consts.iter().copied().collect()
    }

    /// The evaluation environment for `state`: the model's constants, then the
    /// state's variables on top (so a state variable SHADOWS a same-named
    /// constant — exactly the order the by-name entry points have always used).
    ///
    /// Public because the env is a pure function of `(consts, pre-state)`, so it
    /// is IDENTICAL across every action and every invariant evaluated at one
    /// state. A driver that evaluates many of them at one state — the BFS in
    /// [`crate::interp`] — builds it once here and passes it to
    /// [`Model::successors_in`] / `Invariant::expr.eval`, instead of paying a
    /// fresh `BTreeMap` allocation plus a full re-insert per (state, action) and
    /// per (state, invariant). One full-registry BMC sweep built ~1.6M throwaway
    /// maps this way (~109k states x ~8.9 actions + ~5.8 invariants); hoisting it
    /// takes the sweep from 2.54 s to 0.75 s in release, byte-identical results.
    // Skip: same audited `collect`/`extend` alloc class as `const_env` and
    // `init_state`. Model-derivation machinery, same tier as `successors`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn eval_env(&self, state: &BTreeMap<&'static str, i64>) -> BTreeMap<&'static str, i64> {
        let mut env = self.const_env();
        env.extend(state.iter().map(|(k, v)| (*k, *v)));
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

    /// Generate the module with a PARAMETERIZED `Init`: each variable starts at a
    /// fresh CONSTANT `<var>_init`. This lets any predecessor state be the start —
    /// used for strict per-transition conformance against real code (Tier 1):
    /// validating a two-step trace `[prev, next]` with `Init` pinned to `prev`
    /// strictly checks that `Next` admits the real `prev -> next` transition.
    pub fn transition_spec(&self) -> String {
        let mut const_names: Vec<String> =
            self.consts.iter().map(|(n, _)| (*n).to_string()).collect();
        for v in &self.vars {
            const_names.push(format!("{}_init", v.name));
        }
        let init = self
            .vars
            .iter()
            .map(|v| format!("{} = {}_init", v.name, v.name))
            .collect::<Vec<_>>()
            .join(" /\\ ");
        self.render(&const_names, &init)
    }

    /// Shared renderer: emit the module given the CONSTANT names and the `Init`
    /// body. Actions, `Next`, `Spec`, and invariants are identical across the
    /// concrete and parameterized-`Init` forms — one source for both.
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
    /// This closes the gap the original audit documented: the safety models
    /// verify reachable-bad-STATE properties, but a blocking-call deadlock (the
    /// `drain_buffered` `fill_buf` hang — server parked reading more input while
    /// the client is parked awaiting the reply) is a two-party all-blocked wedge
    /// that only `CHECK_DEADLOCK` catches. The default `to_cfg`/`to_cfg_with` keep
    /// `CHECK_DEADLOCK FALSE`, so the 14 existing models — which legitimately stop
    /// at bounded `MaxSeq`/`MaxN` terminals — are untouched.
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
    /// the small exhaustive-check bound) and pins each `<var>_init` to `init`. Used
    /// for per-transition conformance — `ty trace validate --spec` then strictly
    /// checks the real `prev -> next` step against the derived `Next`.
    // Skip: BTreeSet/BTreeMap keyed build + `Extend`/format (absent std
    // bodies). Spec-model cfg emission, same tier as `to_tla`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn transition_cfg(
        &self,
        init: &BTreeMap<&'static str, i64>,
        overrides: &[(&'static str, i64)],
    ) -> String {
        let mut s = String::new();
        for (n, default) in &self.consts {
            let val = overrides
                .iter()
                .find(|(o, _)| o == n)
                .map(|(_, v)| *v)
                .unwrap_or(*default);
            s.push_str(&format!("CONSTANT {n} = {val}\n"));
        }
        for v in &self.vars {
            let val = init.get(v.name).copied().unwrap_or(v.init);
            s.push_str(&format!("CONSTANT {}_init = {val}\n", v.name));
        }
        s.push_str("SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n");
        s
    }

    /// The initial concrete state (variable -> value).
    // Skip: `collect::<BTreeMap>` bottoms out at tree-node allocation (abort,
    // not panic, on OOM — but the toolchain cannot yet see through the
    // generic collect) and `&'static str` Ord (total). Audited-alloc class;
    // verify-only. Revisit when the T3 collect layer lands.
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
            // Deterministic update (`var' = expr`, the overwhelming majority —
            // the registry averages 3.18 updates per action): exactly one
            // candidate, so this step multiplies the product by 1. Write it into
            // every accumulated state IN PLACE rather than cloning the whole
            // accumulator into a fresh `Vec` and dropping the old one — an
            // action with k deterministic updates used to build and discard k
            // maps to produce 1 successor. Result is identical: `insert` on the
            // existing map yields exactly what clone-then-insert did (`BTreeMap`
            // orders by KEY, not insertion), and accumulator order is preserved
            // 1:1, so successor ORDER — which `fire` reads as the canonical
            // lower-bound representative — is unchanged. Worth -20% on a
            // full-registry BMC sweep on top of the `eval_env` hoist.
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
        // One spelling of "the evaluation environment" for every entry point —
        // see `eval_env`; this is the same consts-then-state map as before.
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
}

// ---- The model catalog: the *_model() data constructors, split by family ----
// (pure code motion). The `pub use` re-exports keep every existing
// `crate::derive::*_model` path — and the xref registry — compiling unchanged.
mod models_core;
mod models_effects;
mod models_fx;
mod models_glyphs;
mod models_gui;
mod models_misc;
mod models_native;
mod models_release;
mod models_render;
mod models_session;
mod models_title_summary;
mod models_update;

pub use models_core::*;
pub use models_effects::*;
pub use models_fx::*;
pub use models_glyphs::*;
pub use models_gui::*;
pub use models_misc::*;
pub use models_native::*;
pub use models_release::*;
pub use models_render::*;
pub use models_session::*;
pub use models_title_summary::*;
pub use models_update::*;

/// Property-combinator generators: each returns a fully-formed, `Buggy`-gated
/// [`Model`] (prove@Buggy=0, counterexample@Buggy=1) for a recurring property
/// CLASS. A new property is a struct literal + one registry/harness line, not 50
/// lines of `Expr` constructors. Every name (model / action / var / invariant) is
/// threaded through, so the emitted TLA+ is whatever the author wants — the 7
/// introspection models below are byte-identical instances of these generators.
///
/// The classes (the recurring shapes of the hand-built models): lifecycle/no-leak,
/// gated-completeness, happens-before, teardown-clears, two-stage-leak (info-flow /
/// reply-fidelity), and liveness/no-wedge.
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
                                eq(cst("Buggy"), int(1)),
                                var(p.reg),
                                sub(var(p.reg), int(1)),
                            ),
                        },
                    ],
                },
            ],
            invariants: vec![Invariant {
                name: p.inv,
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
        // empty flag list; the empty conjunction is logically TRUE (`AND() ≡ TRUE`,
        // and the invariant `gate=0 \/ AND(flag=0)` then degenerates to TRUE), so
        // `unwrap_or_else(|| bool_lit(true))` is the mathematically-correct total
        // completion. Every caller supplies a non-empty flag list, so on all real
        // inputs this is identical to the former `.expect(...)` — but the total
        // form carries no panic obligation, discharging the Trust L0 panic-freedom
        // check that `expect` leaves unmodeled in default mode.
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
                expr: or_(eq(var(p.gate), int(0)), closed),
            }],
        }
    }

    // ---- CLASS 5: two-stage leak (info-flow / reply-fidelity) ----
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
    /// stage VALUE (secrecy) OR on `Buggy` directly (reply-fidelity).
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
            invariants: vec![Invariant {
                name: p.inv,
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
                expr: or_(neq(var(p.decided), int(2)), all_hold),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tla_check::TlaSpec;

    /// Guard the brittle `.replace()` in `to_cfg_deadlock_with`: the deadlock cfg
    /// flips CHECK_DEADLOCK to TRUE, and the DEFAULT cfg path stays FALSE (so the
    /// 14 existing models that rely on FALSE are untouched). If `to_cfg_with`'s
    /// literal ever changes, this fails loudly rather than silently no-op'ing.
    #[test]
    fn deadlock_cfg_flips_the_line() {
        let m = forward_handshake_model();
        let dl = m.to_cfg_deadlock_with(&[]);
        assert!(
            dl.contains("CHECK_DEADLOCK TRUE\n"),
            "deadlock cfg must enable the check:\n{dl}"
        );
        assert!(
            !dl.contains("CHECK_DEADLOCK FALSE"),
            "deadlock cfg must not also disable it:\n{dl}"
        );
        assert!(
            m.to_cfg().contains("CHECK_DEADLOCK FALSE\n"),
            "default cfg path unchanged"
        );
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
    fn ring_generates_expected_tla() {
        let tla = ring_model().to_tla();
        // Spot-check the mechanical translation (<= => =<, if => IF/THEN/ELSE).
        assert!(tla.contains("---- MODULE Ring ----"), "{tla}");
        assert!(tla.contains("CONSTANT MaxSeq, Cap"), "{tla}");
        assert!(tla.contains("VARIABLES seq, lo"), "{tla}");
        assert!(tla.contains("Init == seq = 0 /\\ lo = 1"), "{tla}");
        assert!(
            tla.contains("Push == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ lo' = (IF seq + 1 - lo + 1 > Cap THEN lo + 1 ELSE lo)"),
            "{tla}"
        );
        assert!(tla.contains("Next == Push"), "{tla}");
        assert!(tla.contains("Spec == Init /\\ [][Next]_vars"), "{tla}");
        assert!(tla.contains("LenBounded == seq - lo + 1 =< Cap"), "{tla}");
    }

    #[test]
    fn generated_tla_parses_and_exposes_its_defs() {
        // The generated module round-trips through the TLA+ parser, and the
        // action + invariant names are visible (cross-check generator <-> parser).
        let tla = ring_model().to_tla();
        let spec = TlaSpec::parse_str(&tla, "Ring.tla").expect("generated TLA+ must parse");
        assert_eq!(spec.module_name, "Ring");
        assert!(spec.actions.contains("Push"), "defs: {:?}", spec.actions);
        assert!(
            spec.actions.contains("LenBounded"),
            "defs: {:?}",
            spec.actions
        );
    }

    #[test]
    fn transition_spec_parameterizes_init_but_shares_the_action() {
        let m = ring_model();
        let tla = m.transition_spec();
        assert!(
            tla.contains("CONSTANT MaxSeq, Cap, seq_init, lo_init"),
            "{tla}"
        );
        assert!(
            tla.contains("Init == seq = seq_init /\\ lo = lo_init"),
            "{tla}"
        );
        // The action / Next / Spec are the SAME source as the concrete form.
        assert!(
            tla.contains("Push == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ lo' = (IF"),
            "{tla}"
        );
        let mut init = BTreeMap::new();
        init.insert("seq", 42i64);
        init.insert("lo", 1i64);
        let cfg = m.transition_cfg(&init, &[("Cap", 65536), ("MaxSeq", 1_000_000)]);
        assert!(cfg.contains("CONSTANT Cap = 65536"), "{cfg}");
        assert!(cfg.contains("CONSTANT MaxSeq = 1000000"), "{cfg}");
        assert!(cfg.contains("CONSTANT seq_init = 42"), "{cfg}");
        assert!(cfg.contains("CONSTANT lo_init = 1"), "{cfg}");
    }

    #[test]
    fn cursor_model_emits_unchanged_and_disjunctive_next() {
        // The paths the ring never exercises: partial updates -> UNCHANGED, and
        // two actions -> a disjunctive Next.
        let tla = cursor_model().to_tla();
        assert!(
            tla.contains("Grow == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ UNCHANGED << cursor >>"),
            "{tla}"
        );
        assert!(
            tla.contains("Deliver == seq > cursor /\\ cursor' = seq /\\ UNCHANGED << seq >>"),
            "{tla}"
        );
        assert!(tla.contains("Next == Grow \\/ Deliver"), "{tla}");
        assert!(tla.contains("CursorBounded == cursor =< seq"), "{tla}");
    }

    #[test]
    fn cursor_interpreter_holds_invariant_under_both_actions() {
        let m = cursor_model();
        let mut st = m.init_state();
        // Interleave writer growth and reader delivery; the reader must never pass
        // the writer, and `Deliver` must leave `seq` UNCHANGED, `Grow` leave `cursor`.
        for _ in 0..3 {
            let before_cursor = st[&"cursor"];
            assert!(m.fire("Grow", &mut st));
            assert_eq!(
                st[&"cursor"], before_cursor,
                "Grow must leave cursor UNCHANGED"
            );
            assert!(m.check_invariant("CursorBounded", &st));
            let before_seq = st[&"seq"];
            assert!(m.fire("Deliver", &mut st));
            assert_eq!(st[&"seq"], before_seq, "Deliver must leave seq UNCHANGED");
            assert_eq!(
                st[&"cursor"], st[&"seq"],
                "Deliver catches the reader up to the writer"
            );
            assert!(m.check_invariant("CursorBounded", &st));
        }
        // Deliver is guarded by seq > cursor; once caught up it cannot fire.
        assert!(
            !m.fire("Deliver", &mut st),
            "Deliver guard (seq > cursor) blocks when caught up"
        );
    }

    #[test]
    fn subscribe_emits_parenthesized_disjunction_guard_and_eq() {
        let tla = subscribe_model().to_tla();
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
    fn action_enabled_reflects_guards() {
        let m = subscribe_model(); // Buggy = 0
        let behind: BTreeMap<&'static str, i64> =
            [("seq", 5), ("lo", 3), ("cursor", 0), ("lost", 0)]
                .into_iter()
                .collect();
        assert!(
            m.action_enabled("PollGap", &behind),
            "behind reader: PollGap enabled"
        );
        assert!(
            !m.action_enabled("PollDeliver", &behind),
            "behind reader: PollDeliver disabled"
        );
        let caught: BTreeMap<&'static str, i64> =
            [("seq", 5), ("lo", 3), ("cursor", 5), ("lost", 0)]
                .into_iter()
                .collect();
        assert!(
            !m.action_enabled("PollGap", &caught),
            "caught-up reader: PollGap disabled"
        );
        assert!(
            m.action_enabled("PollDeliver", &caught),
            "caught-up reader: PollDeliver enabled"
        );
    }

    #[test]
    fn subscribe_interpreter_enforces_no_silent_loss() {
        let m = subscribe_model(); // committed Buggy = 0
        let mut st = m.init_state();
        // Writer races ahead, evicting past the idle reader (cursor stays 0).
        for _ in 0..4 {
            assert!(m.fire("Grow", &mut st));
        }
        assert_eq!(st[&"seq"], 4);
        assert!(
            st[&"lo"] > st[&"cursor"] + 1,
            "reader has fallen behind the live window"
        );
        // A correct reader CANNOT silently deliver — the guard forbids it; it must
        // gap. `lost` stays 0.
        assert!(
            !m.fire("PollDeliver", &mut st),
            "a behind reader must not silently deliver (Buggy=0)"
        );
        assert!(
            m.fire("PollGap", &mut st),
            "a behind reader resyncs via gap"
        );
        assert_eq!(
            st[&"cursor"], st[&"seq"],
            "gap resyncs the cursor to the head"
        );
        assert!(m.check_invariant("NoSilentLoss", &st));
        assert_eq!(st[&"lost"], 0);
        // Caught up now: delivery is allowed and still loses nothing.
        assert!(m.fire("PollDeliver", &mut st));
        assert!(m.check_invariant("NoSilentLoss", &st));
    }

    #[test]
    fn transact_emits_conjunctive_guards() {
        let tla = transact_model().to_tla();
        assert!(
            tla.contains(
                "CommitClean == active = 1 /\\ seq = tbase /\\ seq =< MaxSeq - K /\\ \
                 seq' = seq + K /\\ active' = 0 /\\ UNCHANGED << tbase, lost >>"
            ),
            "{tla}"
        );
        assert!(
            tla.contains("Next == Write \\/ Begin \\/ CommitClean \\/ Abort \\/ BuggyCommit"),
            "{tla}"
        );
        assert!(tla.contains("NoLostUpdate == lost = 0"), "{tla}");
    }

    #[test]
    fn transact_interpreter_no_lost_update() {
        let m = transact_model(); // committed Buggy = 0
        let mut st = m.init_state();
        assert!(m.fire("Begin", &mut st)); // txn reads base = 0
        assert_eq!(st[&"tbase"], 0);
        assert!(m.fire("Write", &mut st)); // a concurrent write advances the head
        assert_eq!(st[&"seq"], 1);
        // Conflict (seq > tbase): a correct txn must NOT commit, and the buggy
        // commit is disabled at Buggy=0 — so no update can be lost.
        assert!(
            !m.fire("CommitClean", &mut st),
            "must not commit-clean under a conflict"
        );
        assert!(
            !m.fire("BuggyCommit", &mut st),
            "buggy commit is disabled at Buggy=0"
        );
        assert!(
            m.fire("Abort", &mut st),
            "the correct path aborts the conflicted txn"
        );
        assert_eq!(st[&"active"], 0);
        assert!(m.check_invariant("NoLostUpdate", &st));
        assert_eq!(st[&"lost"], 0);
        // A clean txn (no intervening write) commits K atomically.
        assert!(m.fire("Begin", &mut st)); // base = seq = 1
        assert!(m.fire("CommitClean", &mut st));
        assert_eq!(st[&"seq"], 1 + 2, "clean commit advances by K");
        assert!(m.check_invariant("NoLostUpdate", &st));
    }

    #[test]
    fn kernel_and_snapshot_generate_expected_tla() {
        let k = kernel_model().to_tla();
        assert!(
            k.contains("Emit == seq =< MaxSeq - 1 /\\ count' = count + 1 /\\ seq' = (IF Buggy = 1 THEN seq + 2 ELSE seq + 1)"),
            "{k}"
        );
        assert!(k.contains("Next == Emit"), "{k}");
        assert!(k.contains("SeqIsCount == seq = count"), "{k}");
        let s = snapshot_model().to_tla();
        assert!(
            s.contains("Write == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ leaked' = (IF Buggy = 1 /\\ snapped = 1 THEN 1 ELSE leaked) /\\ UNCHANGED << snapped >>"),
            "{s}"
        );
        assert!(s.contains("SnapshotIsolated == leaked = 0"), "{s}");
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
    fn kernel_interpreter_keeps_seq_eq_count() {
        let m = kernel_model();
        let mut st = m.init_state();
        for i in 1..=5 {
            assert!(m.fire("Emit", &mut st));
            assert_eq!(st[&"seq"], i, "gap-free: seq advances by exactly 1");
            assert_eq!(st[&"count"], i);
            assert!(m.check_invariant("SeqIsCount", &st));
        }
        assert!(!m.fire("Emit", &mut st), "guard bounds seq at MaxSeq");
    }

    #[test]
    fn snapshot_interpreter_isolates_from_later_writes() {
        let m = snapshot_model();
        let mut st = m.init_state();
        assert!(m.fire("Write", &mut st)); // pre-snapshot write
        assert!(m.fire("Snap", &mut st));
        assert_eq!(st[&"snapped"], 1);
        assert!(m.fire("Write", &mut st)); // post-snapshot write must not leak (Buggy=0)
        assert!(m.check_invariant("SnapshotIsolated", &st));
        assert_eq!(
            st[&"leaked"], 0,
            "a later write did not leak into the snapshot"
        );
        assert!(
            !m.fire("Snap", &mut st),
            "only one snapshot (guard snapped = 0)"
        );
    }

    #[test]
    fn interpreter_matches_ring_semantics_and_holds_invariant() {
        // The executable twin: drive the SAME model and check the ring discipline
        // (seq monotone +1; lo advances exactly at the cap) and the invariant.
        let m = ring_model();
        let mut st = m.init_state();
        assert_eq!(st[&"seq"], 0);
        assert_eq!(st[&"lo"], 1);
        let cap = 3;
        let mut fired = 0;
        while m.fire("Push", &mut st) {
            fired += 1;
            assert_eq!(st[&"seq"], fired, "seq must be monotone +1");
            // lo stays 1 until the window exceeds Cap, then tracks the head.
            let expected_lo = if fired - 1 + 1 > cap {
                fired - cap + 1
            } else {
                1
            };
            assert_eq!(
                st[&"lo"],
                expected_lo.max(1),
                "lo eviction discipline at seq={fired}"
            );
            assert!(
                m.check_invariant("LenBounded", &st),
                "LenBounded must hold at seq={fired}"
            );
        }
        // Guard `seq <= MaxSeq-1` (MaxSeq=6) stops Push after seq reaches 6.
        assert_eq!(st[&"seq"], 6, "guard must bound seq at MaxSeq");
    }
}
