// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Derived TLA+: one authored model, two faithful backends.

use std::collections::BTreeMap;

/// A value in the model's (bounded-integer / boolean) semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bool(bool),
}

impl Value {
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "expected Int, got Bool")
    )]
    pub(crate) fn as_int(self) -> i64 {
        match self {
            Value::Int(i) => i,
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
            Value::Int(_) => panic!("expected Bool, got Int — model type error"),
        }
    }
}

/// A small expression language shared by the TLA+ generator and the interpreter.
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
    /// `a \/ b`; emits `(a \/ b)`, parenthesized because a guard is conjoined with the update conjuncts and `/\` binds tighter.
    Or(Box<Expr>, Box<Expr>),
    /// `a /\ b`; emits `a /\ b` unparenthesized, which composes with the action-level conjunction it joins.
    And(Box<Expr>, Box<Expr>),
    /// `IF cond THEN a ELSE b` (cond boolean; arms integer).
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `a <=> b` (boolean iff) — emits parenthesized `(a <=> b)`.
    Iff(Box<Expr>, Box<Expr>),
    /// `\A n \in lo..hi : body` (universal quantifier; `body` boolean, may reference the bound index by name).
    Forall(&'static str, Box<Expr>, Box<Expr>, Box<Expr>),
    /// `fn[index]` — access a function-valued (`[1..N -> BOOLEAN]`) state variable.
    FnAccess(&'static str, Box<Expr>),
    /// `[fn EXCEPT ![index] = value]` — a point update of a function variable.
    Except(&'static str, Box<Expr>, Box<Expr>),
    /// `[n \in lo..hi |-> body]`.
    Comprehension(&'static str, Box<Expr>, Box<Expr>, Box<Expr>),
    /// `TRUE` / `FALSE`.
    Bool(bool),
    /// `a # b` (integer inequality).
    Neq(Box<Expr>, Box<Expr>),
    /// `lo..hi`, a bounded integer set and the only nondeterministic construct.
    InRange(Box<Expr>, Box<Expr>),
}

/// Convenience constructors (keep model definitions terse).
#[inline]
pub const fn int(i: i64) -> Expr {
    Expr::Int(i)
}
#[inline]
pub const fn var(n: &'static str) -> Expr {
    Expr::Var(n)
}
#[inline]
pub const fn cst(n: &'static str) -> Expr {
    Expr::ConstRef(n)
}
#[inline]
pub fn add(a: Expr, b: Expr) -> Expr {
    Expr::Add(Box::new(a), Box::new(b))
}
#[inline]
pub fn sub(a: Expr, b: Expr) -> Expr {
    Expr::Sub(Box::new(a), Box::new(b))
}
#[inline]
pub fn gt(a: Expr, b: Expr) -> Expr {
    Expr::Gt(Box::new(a), Box::new(b))
}
#[inline]
pub fn le(a: Expr, b: Expr) -> Expr {
    Expr::Le(Box::new(a), Box::new(b))
}
#[inline]
pub fn eq(a: Expr, b: Expr) -> Expr {
    Expr::Eq(Box::new(a), Box::new(b))
}
#[inline]
pub fn or_(a: Expr, b: Expr) -> Expr {
    Expr::Or(Box::new(a), Box::new(b))
}
#[inline]
pub fn and_(a: Expr, b: Expr) -> Expr {
    Expr::And(Box::new(a), Box::new(b))
}
#[inline]
pub fn if_(c: Expr, a: Expr, b: Expr) -> Expr {
    Expr::If(Box::new(c), Box::new(a), Box::new(b))
}
#[inline]
pub fn iff(a: Expr, b: Expr) -> Expr {
    Expr::Iff(Box::new(a), Box::new(b))
}
#[inline]
pub fn forall(idx: &'static str, lo: Expr, hi: Expr, body: Expr) -> Expr {
    Expr::Forall(idx, Box::new(lo), Box::new(hi), Box::new(body))
}
#[inline]
pub fn fn_access(f: &'static str, index: Expr) -> Expr {
    Expr::FnAccess(f, Box::new(index))
}
#[inline]
pub fn except(f: &'static str, index: Expr, value: Expr) -> Expr {
    Expr::Except(f, Box::new(index), Box::new(value))
}
#[inline]
pub fn comprehension(idx: &'static str, lo: Expr, hi: Expr, body: Expr) -> Expr {
    Expr::Comprehension(idx, Box::new(lo), Box::new(hi), Box::new(body))
}
#[inline]
pub const fn bool_lit(b: bool) -> Expr {
    Expr::Bool(b)
}
#[inline]
pub fn neq(a: Expr, b: Expr) -> Expr {
    Expr::Neq(Box::new(a), Box::new(b))
}
/// `lo..hi`, the nondeterministic update RHS ([`Expr::InRange`]).
#[inline]
pub fn in_range(lo: Expr, hi: Expr) -> Expr {
    Expr::InRange(Box::new(lo), Box::new(hi))
}

impl Expr {
    /// Evaluate against an environment (state variables + constants).
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
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn eval(&self, env: &BTreeMap<&'static str, i64>) -> Value {
        match self {
            Expr::Int(i) => Value::Int(*i),
            Expr::Var(n) | Expr::ConstRef(n) => Value::Int(match env.get(n) {
                Some(v) => *v,
                None => panic!("unbound identifier `{n}` in model evaluation"),
            }),
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
            Expr::FnAccess(..) | Expr::Except(..) | Expr::Comprehension(..) => {
                panic!(
                    "function-valued Expr is TLA+-generation only (Tier-0 ty-checked, not interpreter-evaluable)"
                )
            }
            Expr::InRange(..) => panic!(
                "InRange (`lo..hi`) is set-valued; it is meaningful only as a nondeterministic \
                 update RHS — fire the action via `Model::successors`, not scalar `eval`"
            ),
        }
    }

    /// Render as a TLA+ expression.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn to_tla(&self) -> String {
        match self {
            Expr::Int(i) => i.to_string(),
            Expr::Var(n) | Expr::ConstRef(n) => (*n).to_string(),
            Expr::Add(a, b) => format!("{} + {}", a.to_tla(), b.to_tla()),
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
            Expr::Or(a, b) => format!("({} \\/ {})", a.to_tla(), b.to_tla()),
            Expr::And(a, b) => format!("{} /\\ {}", a.to_tla(), b.to_tla()),
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

/// A guarded action (a disjunct of `Next`).
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

/// What an invariant states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantKind {
    /// `invariant`: a design claim, which some mutant of the model must break or it asserts nothing (`verify::sweep_faults`).
    Claim,
    /// `bound`: the box the checkers walk, which no mutant is expected to break.
    Bound,
}

/// A named liveness obligation over a [`Model`]: `[]<>goal` under the fairness it states, so no fair run leaves the goal for good.
#[derive(Debug, Clone)]
pub struct Liveness {
    /// The property's name; `ty` checks it as the operator of this name, and `<name>Spec` is the fair specification it is checked under.
    pub name: &'static str,
    /// The goal: a boolean expression over the model's variables and constants.
    pub goal: Expr,
    /// Weakly fair actions (`WF_vars`): taken once they stay enabled.
    pub weak: Vec<&'static str>,
    /// Strongly fair actions (`SF_vars`): taken once they keep coming enabled.
    pub strong: Vec<&'static str>,
    /// The `Buggy` mutants (actions dead at the committed config), each of which, added alone to the `Buggy = 1` baseline, must break the property.
    pub mutants: Vec<&'static str>,
}

impl Liveness {
    /// Whether the goal holds at `state`, under `m`'s constants.
    ///
    /// # Panics
    /// If the goal is not boolean-typed — a model type error, failed closed like every other ill-typed evaluation in this module.
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

    /// `m`'s TLA+ module with the fair specification and the property appended.
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

    /// The `.cfg` that checks the property alone: constants (with `overrides`), the fair specification, the `PROPERTY`, and no invariant.
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

/// A function-valued state variable `[1..range -> BOOLEAN]`, initialized all-FALSE, for per-element models (a ring's live-set) the scalar projection cannot express.
#[derive(Debug, Clone)]
pub struct FnVar {
    pub name: &'static str,
    /// Upper bound of the index domain (a CONSTANT name); the domain is `1..range`.
    pub range: &'static str,
}

/// A bounded state machine — the single source for both the TLA+ spec and the executable semantics.
#[derive(Debug, Clone)]
pub struct Model {
    pub name: &'static str,
    pub consts: Vec<(&'static str, i64)>,
    pub vars: Vec<StateVar>,
    /// Function-valued variables (`[1..range -> BOOLEAN]`).
    pub fn_vars: Vec<FnVar>,
    pub actions: Vec<Action>,
    pub invariants: Vec<Invariant>,
}

impl Model {
    /// All state-variable names, scalar then function-valued (the order used in
    /// `VARIABLES`, `vars`, and `UNCHANGED`).
    #[cfg_attr(trust_verify, trust::skip)]
    fn all_var_names(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.vars.iter().map(|x| x.name).collect();
        v.extend(self.fn_vars.iter().map(|f| f.name));
        v
    }

    /// The evaluation environment for `state`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn eval_env(&self, state: &BTreeMap<&'static str, i64>) -> BTreeMap<&'static str, i64> {
        let mut env = state.clone();
        for (k, v) in self.consts.iter().rev() {
            env.entry(*k).or_insert(*v);
        }
        env
    }

    /// Generate the complete TLA+ module text (the embedded, `ty`-checkable spec) with the model's concrete `Init` (variables start at their declared values).
    pub fn to_tla(&self) -> String {
        let const_names: Vec<String> = self.consts.iter().map(|(n, _)| (*n).to_string()).collect();
        let mut init_parts: Vec<String> = self
            .vars
            .iter()
            .map(|v| format!("{} = {}", v.name, v.init))
            .collect();
        for f in &self.fn_vars {
            init_parts.push(format!("{} = [n \\in 1..{} |-> FALSE]", f.name, f.range));
        }
        let init = init_parts.join(" /\\ ");
        self.render(&const_names, &init)
    }

    /// The module with an open `Init` (`Init == TRUE`), for strict per-transition conformance (Tier 1).
    pub fn transition_spec(&self) -> String {
        let const_names: Vec<String> = self.consts.iter().map(|(n, _)| (*n).to_string()).collect();
        self.render(&const_names, "TRUE")
    }

    /// Emit the module from the CONSTANT names and the `Init` body; the rest is shared by the
    /// concrete and open-`Init` forms.
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

        for a in &self.actions {
            let mut conj: Vec<String> = Vec::new();
            if let Some(g) = &a.guard {
                conj.push(g.to_tla());
            }
            for u in &a.updates {
                match &u.expr {
                    Expr::InRange(lo, hi) => {
                        conj.push(format!("{}' \\in {}..{}", u.var, lo.to_tla(), hi.to_tla()))
                    }
                    _ => conj.push(format!("{}' = {}", u.var, u.expr.to_tla())),
                }
            }
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

    /// Generate the `.cfg`: constant bindings, the specification, and every invariant.
    pub fn to_cfg(&self) -> String {
        self.to_cfg_with(&[])
    }

    /// Like [`to_cfg`](Self::to_cfg) with constant `overrides`, e.g. `Buggy = 1` to check that an invariant is non-trivial (the buggy variant must yield a counterexample).
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

    /// Like [`to_cfg_with`](Self::to_cfg_with) with `CHECK_DEADLOCK TRUE`, for request/reply models whose failure is a two-party wedge (each side parked on the other) that only deadlock detection catches.
    #[must_use]
    pub fn to_cfg_deadlock_with(&self, overrides: &[(&'static str, i64)]) -> String {
        self.to_cfg_with(overrides)
            .replace("CHECK_DEADLOCK FALSE\n", "CHECK_DEADLOCK TRUE\n")
    }

    /// Config for [`transition_spec`](Self::transition_spec).
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
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn init_state(&self) -> BTreeMap<&'static str, i64> {
        self.vars.iter().map(|v| (v.name, v.init)).collect()
    }

    /// The `(machine, action)` anchor targets this model declares (`machine` is the model's `name`).
    pub fn anchors(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.actions.iter().map(move |a| (self.name, a.name))
    }

    /// Every next state reachable by firing `action` from `state`: the executable twin of the TLA+ action's fan-out.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn successors(
        &self,
        action: &str,
        state: &BTreeMap<&'static str, i64>,
    ) -> Vec<BTreeMap<&'static str, i64>> {
        let Some(act) = self.actions.iter().find(|a| a.name == action) else {
            panic!("no action `{action}` in model `{}`", self.name)
        };
        let env = self.eval_env(state);
        self.successors_in(act, &env, state)
    }

    /// [`Model::successors`] with the action resolved and the environment built, as [`crate::interp`]'s BFS calls it (both are loop-invariant across one state).
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
        let choices: Vec<(&'static str, Vec<i64>)> = act
            .updates
            .iter()
            .map(|u| match &u.expr {
                Expr::InRange(lo, hi) => {
                    let lo: &Expr = std::ops::Deref::deref(lo);
                    let hi: &Expr = std::ops::Deref::deref(hi);
                    let (l, h) = (lo.eval(env).as_int(), hi.eval(env).as_int());
                    let mut vals = Vec::new();
                    let mut v = l;
                    while v <= h {
                        vals.push(v);
                        if v == h {
                            break;
                        }
                        v = v.wrapping_add(1);
                    }
                    (u.var, vals)
                }
                e => (u.var, vec![e.eval(env).as_int()]),
            })
            .collect();
        let mut acc: Vec<BTreeMap<&'static str, i64>> = vec![state.clone()];
        for (var, vals) in choices {
            // Deterministic update (the common case): write the one candidate in place instead
            if let [only] = vals[..] {
                for s in &mut acc {
                    s.insert(var, only);
                }
                continue;
            }
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

    /// Fire a named action against `state`, applying its canonical successor.
    pub fn fire(&self, action: &str, state: &mut BTreeMap<&'static str, i64>) -> bool {
        match self.successors(action, state).into_iter().next() {
            Some(next) => {
                *state = next;
                true
            }
            None => false,
        }
    }

    /// Whether `action`'s guard holds in `state` (no guard => enabled).
    #[cfg_attr(trust_verify, trust::contract_panic(message_contains = "no action `"))]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn action_enabled(&self, action: &str, state: &BTreeMap<&'static str, i64>) -> bool {
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
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "no invariant `")
    )]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_invariant(&self, name: &str, state: &BTreeMap<&'static str, i64>) -> bool {
        let Some(inv) = self.invariants.iter().find(|i| i.name == name) else {
            panic!("no invariant `{name}`")
        };
        let env = self.eval_env(state);
        self.check_invariant_in(inv, &env)
    }

    /// [`Model::check_invariant`] with the invariant resolved and the environment built.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn check_invariant_in(&self, inv: &Invariant, env: &BTreeMap<&'static str, i64>) -> bool {
        inv.expr.eval(env).as_bool()
    }

    /// The `Buggy` values the model's mutants are written for.
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

    /// Whether the model has function-valued state, which only `ty` can check: the in-process interpreter evaluates scalar models alone.
    #[must_use]
    pub fn is_function_valued(&self) -> bool {
        !self.fn_vars.is_empty()
    }

    /// Why each declared bound is not a range, one line per fault.
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
/// variable.
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
/// each variable its atoms pin to one value or none.
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

/// Property-combinator generators.
pub mod props {
    use super::*;

    /// Loosen the buggy case: `(Buggy = 1 \/ cond)`, so Buggy admits more (ordering).
    fn or_buggy(cond: Expr) -> Expr {
        or_(eq(cst("Buggy"), int(1)), cond)
    }
    /// Restrict to `cond` only when buggy: `(Buggy = 0 \/ cond)`.
    fn or_correct(cond: Expr) -> Expr {
        or_(eq(cst("Buggy"), int(0)), cond)
    }

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
    /// `acquire` bumps both counters (bounded by `max`).
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
    /// `pick` fans `item' \in 0..domain_max`.
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

    pub struct Ordering {
        pub name: &'static str,
        pub a: &'static str,
        pub a_act: &'static str,
        pub b: &'static str,
        pub b_act: &'static str,
        pub inv: &'static str,
    }
    /// `a_act` sets `a:=1`; `b_act` (guard `b=0 /\ (Buggy=1 \/ a=1)`) sets `b:=1`.
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

    pub struct Teardown {
        pub name: &'static str,
        pub flags: Vec<&'static str>,
        pub gate: &'static str,
        pub act: &'static str,
        pub inv: &'static str,
    }
    /// `act` (guard `gate=0`) drops every flag to 0 (Buggy leaves them 1) and sets `gate:=1`.
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
    /// `stage_act` (guard `stage=0`) sets `stage := stage_rhs`.
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
    /// Two-party request→relay→reply.
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
                kind: InvariantKind::Bound,
                expr: le(var(p.waiting), int(1)),
            }],
        }
    }

    pub struct ConjunctiveAuthz {
        pub name: &'static str,
        pub guards: Vec<&'static str>,
        pub decided: &'static str,
        pub pick: &'static str,
        pub decide: &'static str,
        pub drop: &'static str,
        pub inv: &'static str,
    }
    /// `pick` sets each guard nondeterministically in `{0,1}`.
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

/// Single-hold predictive echo reconciliation across `ExtractSnapshot`, `PtyEchoArrives`, `Reconcile` and `PaintFrame`.
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

/// Formal model of PTY parse-thread end-of-batch yielding to a waiting UI thread (`crates/aterm-gui/src/spawn.rs`).
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

/// Formal model of `WarmTier` $O(1)$ cumulative-line offset eviction (`crates/aterm-scrollback/src/tier.rs`).
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
        let env = BTreeMap::new();

        let e = sub(int(5), sub(int(2), int(1)));
        assert_eq!(e.to_tla(), "5 - (2 - 1)", "Sub RHS must be parenthesized");
        assert_eq!(e.eval(&env).as_int(), 4);

        let e2 = sub(int(5), add(int(2), int(1)));
        assert_eq!(
            e2.to_tla(),
            "5 - (2 + 1)",
            "Sub RHS Add must be parenthesized"
        );
        assert_eq!(e2.eval(&env).as_int(), 2);

        let e3 = sub(sub(int(5), int(2)), int(1));
        assert_eq!(e3.to_tla(), "5 - 2 - 1", "left-nested Sub needs no parens");
        assert_eq!(e3.eval(&env).as_int(), 2);
    }

    /// An ill-typed Expr panics, as `eval`'s unbound-identifier arm does, never evaluating to a
    /// default.
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
