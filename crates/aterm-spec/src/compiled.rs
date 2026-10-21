// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A [`Model`] resolved once for the interpreter's state-space walks.
//!
//! [`Expr::eval`] looks every identifier up in a `BTreeMap<&str, i64>` environment rebuilt at
//! each step, and a walk evaluates the same few expressions millions of times over states that
//! bind the same variables, so this module resolves names once. A state becomes its values in
//! sorted-name order (the order a `BTreeMap` state iterates, and the identity
//! [`crate::interp`]'s `seen` sets key on), a variable its index into that slice, a constant its
//! value, and a `\A` index a slot on a small local stack.
//!
//! The semantics are [`Expr::eval`]'s and [`Model::successors_in`]'s, case for case, so a walk
//! visits the same states in the same order and returns the same verdicts:
//!
//! * A `\A` index shadows a state variable, which shadows a constant (the layering of
//!   `eval_env` and `Forall`); a constant declared twice takes its last declaration.
//! * Evaluation order, short-circuiting and wrapping arithmetic are `eval`'s, and each panic
//!   (unbound identifier, type error, function- or set-valued expression in a scalar position)
//!   is raised lazily, with `eval`'s message, only when the expression is evaluated.
//! * Successors: guard first, every update evaluated against the pre-state in declaration
//!   order, then the cartesian product with each nondeterministic `lo..hi` ascending.
//!
//! A model with an update to a name its initial state does not bind (which grows a `BTreeMap`
//! state by a key) is not compiled ([`Compiled::new`] returns `None`); the caller walks it the
//! original way.

use std::collections::BTreeMap;

use crate::derive::{Expr, Model, Value};

/// What a scalar position reads, resolved.
#[derive(Debug)]
enum Node {
    Int(i64),
    Bool(bool),
    /// A state variable: its index in the sorted-name value slice.
    Slot(usize),
    /// A `\A` index: its depth on the local stack.
    Local(usize),
    /// An identifier nothing binds: panics with `eval`'s message when read.
    Unbound(&'static str),
    Add(Box<Node>, Box<Node>),
    Sub(Box<Node>, Box<Node>),
    Gt(Box<Node>, Box<Node>),
    Le(Box<Node>, Box<Node>),
    Eq(Box<Node>, Box<Node>),
    Neq(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    And(Box<Node>, Box<Node>),
    If(Box<Node>, Box<Node>, Box<Node>),
    Iff(Box<Node>, Box<Node>),
    /// `\A`: bounds, then the body with its index at local depth `.0`.
    Forall(usize, Box<Node>, Box<Node>, Box<Node>),
    /// A function-valued expression in a scalar position (`eval` panics).
    FnValued,
    /// A `lo..hi` set in a scalar position (`eval` panics).
    SetValued,
    /// `Gt`/`Le`/`Eq`/`Neq` of two atoms: one step where the tree takes three.
    Cmp(Cmp, Atom, Atom),
    /// `Add`/`Sub` of two atoms, wrapping as `eval`'s.
    Arith(Arith, Atom, Atom),
}

/// An integer operand that reads without evaluating anything: a constant, a state variable,
/// a `\A` index. Reading one can neither panic nor yield a `Bool`, so an operation over two of
/// them is `eval`'s operation over two `Int`s, in one step.
#[derive(Debug, Clone, Copy)]
enum Atom {
    Int(i64),
    Slot(usize),
    Local(usize),
}

impl Atom {
    /// What `node` reads, when it is an atom.
    fn of(node: &Node) -> Option<Self> {
        match node {
            Node::Int(i) => Some(Atom::Int(*i)),
            Node::Slot(i) => Some(Atom::Slot(*i)),
            Node::Local(d) => Some(Atom::Local(*d)),
            _ => None,
        }
    }

    fn read(self, s: &[i64], locals: &[i64]) -> i64 {
        match self {
            Atom::Int(i) => i,
            Atom::Slot(i) => s[i],
            Atom::Local(d) => locals[d],
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Cmp {
    Gt,
    Le,
    Eq,
    Neq,
}

impl Cmp {
    fn holds(self, a: i64, b: i64) -> bool {
        match self {
            Cmp::Gt => a > b,
            Cmp::Le => a <= b,
            Cmp::Eq => a == b,
            Cmp::Neq => a != b,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Arith {
    Add,
    Sub,
}

impl Arith {
    fn apply(self, a: i64, b: i64) -> i64 {
        match self {
            Arith::Add => a.wrapping_add(b),
            Arith::Sub => a.wrapping_sub(b),
        }
    }
}

impl Node {
    // Skip: a recursive evaluator over a boxed tree whose panics are `eval`'s, message for
    // message.
    #[cfg_attr(trust_verify, trust::skip)]
    fn eval(&self, s: &[i64], locals: &mut Vec<i64>) -> Value {
        match self {
            Node::Int(i) => Value::Int(*i),
            Node::Bool(b) => Value::Bool(*b),
            Node::Slot(i) => Value::Int(s[*i]),
            Node::Local(d) => Value::Int(locals[*d]),
            Node::Unbound(n) => panic!("unbound identifier `{n}` in model evaluation"),
            Node::Add(a, b) => Value::Int(
                a.eval(s, locals)
                    .as_int()
                    .wrapping_add(b.eval(s, locals).as_int()),
            ),
            Node::Sub(a, b) => Value::Int(
                a.eval(s, locals)
                    .as_int()
                    .wrapping_sub(b.eval(s, locals).as_int()),
            ),
            Node::Gt(a, b) => Value::Bool(a.eval(s, locals).as_int() > b.eval(s, locals).as_int()),
            Node::Le(a, b) => Value::Bool(a.eval(s, locals).as_int() <= b.eval(s, locals).as_int()),
            Node::Eq(a, b) => Value::Bool(a.eval(s, locals).as_int() == b.eval(s, locals).as_int()),
            Node::Neq(a, b) => {
                Value::Bool(a.eval(s, locals).as_int() != b.eval(s, locals).as_int())
            }
            Node::Or(a, b) => {
                Value::Bool(a.eval(s, locals).as_bool() || b.eval(s, locals).as_bool())
            }
            Node::And(a, b) => {
                Value::Bool(a.eval(s, locals).as_bool() && b.eval(s, locals).as_bool())
            }
            Node::If(c, a, b) => {
                if c.eval(s, locals).as_bool() {
                    a.eval(s, locals)
                } else {
                    b.eval(s, locals)
                }
            }
            Node::Iff(a, b) => {
                Value::Bool(a.eval(s, locals).as_bool() == b.eval(s, locals).as_bool())
            }
            Node::Forall(depth, lo, hi, body) => {
                let (l, h) = (lo.eval(s, locals).as_int(), hi.eval(s, locals).as_int());
                // The index's slot: every enclosing `\A` holds the ones below it.
                locals.truncate(*depth);
                locals.push(0);
                for n in l..=h {
                    locals[*depth] = n;
                    if !body.eval(s, locals).as_bool() {
                        locals.truncate(*depth);
                        return Value::Bool(false);
                    }
                }
                locals.truncate(*depth);
                Value::Bool(true)
            }
            Node::FnValued => panic!(
                "function-valued Expr is TLA+-generation only (Tier-0 ty-checked, not interpreter-evaluable)"
            ),
            Node::SetValued => panic!(
                "InRange (`lo..hi`) is set-valued; it is meaningful only as a nondeterministic \
                 update RHS — fire the action via `Model::successors`, not scalar `eval`"
            ),
            Node::Cmp(op, a, b) => Value::Bool(op.holds(a.read(s, locals), b.read(s, locals))),
            Node::Arith(op, a, b) => Value::Int(op.apply(a.read(s, locals), b.read(s, locals))),
        }
    }
}

/// One update's right-hand side.
#[derive(Debug)]
enum Rhs {
    /// `var' = e`.
    Det(Node),
    /// `var' \in lo..hi`.
    Range(Node, Node),
}

#[derive(Debug)]
struct CAction {
    guard: Option<Node>,
    /// `(slot, rhs)` in declaration order.
    updates: Vec<(usize, Rhs)>,
}

/// A model resolved for the walks in [`crate::interp`]; see the module docs.
#[derive(Debug)]
pub(crate) struct Compiled {
    /// The state variables in sorted order: slot `i` is `names[i]`.
    names: Vec<&'static str>,
    init: Vec<i64>,
    actions: Vec<CAction>,
    invariants: Vec<Node>,
}

/// Name resolution for one model: the scope a compiled expression reads.
struct Scope<'a> {
    slots: &'a BTreeMap<&'static str, usize>,
    consts: &'a BTreeMap<&'static str, i64>,
}

impl Scope<'_> {
    // Skip: recursive tree construction (Box allocation, map lookups).
    #[cfg_attr(trust_verify, trust::skip)]
    fn node(&self, e: &Expr, bound: &mut Vec<&'static str>) -> Node {
        let two = |a: &Expr, b: &Expr, bound: &mut Vec<&'static str>| {
            (Box::new(self.node(a, bound)), Box::new(self.node(b, bound)))
        };
        // An operation over two atoms is one node, and over two constants its value: neither
        // reads anything that can panic, so `eval`'s order and laziness have nothing to keep.
        let fused = |a: &Node, b: &Node| Atom::of(a).zip(Atom::of(b));
        let cmp = |op: Cmp,
                   a: &Expr,
                   b: &Expr,
                   bound: &mut Vec<&'static str>,
                   tree: fn(Box<Node>, Box<Node>) -> Node| {
            let (a, b) = two(a, b, bound);
            match fused(&a, &b) {
                Some((Atom::Int(x), Atom::Int(y))) => Node::Bool(op.holds(x, y)),
                Some((x, y)) => Node::Cmp(op, x, y),
                None => tree(a, b),
            }
        };
        let arith = |op: Arith,
                     a: &Expr,
                     b: &Expr,
                     bound: &mut Vec<&'static str>,
                     tree: fn(Box<Node>, Box<Node>) -> Node| {
            let (a, b) = two(a, b, bound);
            match fused(&a, &b) {
                Some((Atom::Int(x), Atom::Int(y))) => Node::Int(op.apply(x, y)),
                Some((x, y)) => Node::Arith(op, x, y),
                None => tree(a, b),
            }
        };
        match e {
            Expr::Int(i) => Node::Int(*i),
            Expr::Bool(b) => Node::Bool(*b),
            Expr::Var(n) | Expr::ConstRef(n) => {
                if let Some(depth) = bound.iter().rposition(|b| b == n) {
                    Node::Local(depth)
                } else if let Some(&slot) = self.slots.get(n) {
                    Node::Slot(slot)
                } else if let Some(&v) = self.consts.get(n) {
                    Node::Int(v)
                } else {
                    Node::Unbound(n)
                }
            }
            Expr::Add(a, b) => arith(Arith::Add, a, b, bound, Node::Add),
            Expr::Sub(a, b) => arith(Arith::Sub, a, b, bound, Node::Sub),
            Expr::Gt(a, b) => cmp(Cmp::Gt, a, b, bound, Node::Gt),
            Expr::Le(a, b) => cmp(Cmp::Le, a, b, bound, Node::Le),
            Expr::Eq(a, b) => cmp(Cmp::Eq, a, b, bound, Node::Eq),
            Expr::Neq(a, b) => cmp(Cmp::Neq, a, b, bound, Node::Neq),
            Expr::Or(a, b) => {
                let (a, b) = two(a, b, bound);
                Node::Or(a, b)
            }
            Expr::And(a, b) => {
                let (a, b) = two(a, b, bound);
                Node::And(a, b)
            }
            Expr::Iff(a, b) => {
                let (a, b) = two(a, b, bound);
                Node::Iff(a, b)
            }
            Expr::If(c, a, b) => Node::If(
                Box::new(self.node(c, bound)),
                Box::new(self.node(a, bound)),
                Box::new(self.node(b, bound)),
            ),
            Expr::Forall(idx, lo, hi, body) => {
                // The bounds read the enclosing scope; only the body sees `idx`.
                let lo = Box::new(self.node(lo, bound));
                let hi = Box::new(self.node(hi, bound));
                let depth = bound.len();
                bound.push(idx);
                let body = Box::new(self.node(body, bound));
                bound.pop();
                Node::Forall(depth, lo, hi, body)
            }
            Expr::FnAccess(..) | Expr::Except(..) | Expr::Comprehension(..) => Node::FnValued,
            Expr::InRange(..) => Node::SetValued,
        }
    }
}

impl Compiled {
    /// Resolve `m`, or `None` when an update writes a name its initial state
    /// does not bind (see the module docs).
    // Skip: map/Vec construction over the model's declarations.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn new(m: &Model) -> Option<Self> {
        let init_state = m.init_state();
        let names: Vec<&'static str> = init_state.keys().copied().collect();
        let init: Vec<i64> = init_state.values().copied().collect();
        let slots: BTreeMap<&'static str, usize> =
            names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let consts: BTreeMap<&'static str, i64> = m.consts.iter().copied().collect();
        let scope = Scope {
            slots: &slots,
            consts: &consts,
        };
        let mut bound = Vec::new();
        let mut actions = Vec::with_capacity(m.actions.len());
        for a in &m.actions {
            let mut updates = Vec::with_capacity(a.updates.len());
            for u in &a.updates {
                let slot = *slots.get(u.var)?;
                let rhs = match &u.expr {
                    Expr::InRange(lo, hi) => {
                        Rhs::Range(scope.node(lo, &mut bound), scope.node(hi, &mut bound))
                    }
                    e => Rhs::Det(scope.node(e, &mut bound)),
                };
                updates.push((slot, rhs));
            }
            actions.push(CAction {
                guard: a.guard.as_ref().map(|g| scope.node(g, &mut bound)),
                updates,
            });
        }
        let invariants = m
            .invariants
            .iter()
            .map(|i| scope.node(&i.expr, &mut bound))
            .collect();
        Some(Self {
            names,
            init,
            actions,
            invariants,
        })
    }

    /// Resolve an expression over `m`, the model this was compiled from (for
    /// a liveness goal).
    // Skip: same class as `new`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn expr(&self, m: &Model, e: &Expr) -> CompiledExpr {
        let slots: BTreeMap<&'static str, usize> = self
            .names
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i))
            .collect();
        let consts: BTreeMap<&'static str, i64> = m.consts.iter().copied().collect();
        let scope = Scope {
            slots: &slots,
            consts: &consts,
        };
        CompiledExpr(scope.node(e, &mut Vec::new()))
    }

    pub(crate) fn init(&self) -> Vec<i64> {
        self.init.clone()
    }

    /// The index of state variable `name` in a value slice, if it is one.
    // Skip: a slice search over the model's own names.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn slot(&self, name: &str) -> Option<usize> {
        // `names` is a `BTreeMap`'s key order: sorted, so searchable.
        self.names.binary_search(&name).ok()
    }

    /// The state `s` is, as the interpreter's public [`crate::interp::State`].
    // Skip: a `collect` into a map.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn state(&self, s: &[i64]) -> BTreeMap<&'static str, i64> {
        self.names.iter().copied().zip(s.iter().copied()).collect()
    }

    /// `Model::check_invariant_in` for invariant `i` at `s`.
    // Skip: drives the evaluator above.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn holds(&self, i: usize, s: &[i64]) -> bool {
        self.invariants[i].eval(s, &mut Vec::new()).as_bool()
    }

    /// `Model::successors_in` for action `a` at `s`, in the same order.
    // Skip: the cartesian product's Vec bookkeeping, as in `successors_in`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn successors(&self, a: usize, s: &[i64]) -> Vec<Vec<i64>> {
        let act = &self.actions[a];
        let mut locals = Vec::new();
        if act
            .guard
            .as_ref()
            .is_some_and(|g| !g.eval(s, &mut locals).as_bool())
        {
            return Vec::new();
        }
        // Every right-hand side reads the pre-state `s`, so evaluating each just before it is
        // applied matches evaluating all first (same values, order and panics). Each is
        // evaluated even once the product is empty, as `successors_in` does.
        let mut acc: Vec<Vec<i64>> = vec![s.to_vec()];
        for (slot, rhs) in &act.updates {
            match rhs {
                Rhs::Det(e) => {
                    let v = e.eval(s, &mut locals).as_int();
                    for st in &mut acc {
                        st[*slot] = v;
                    }
                }
                Rhs::Range(lo, hi) => {
                    let (l, h) = (
                        lo.eval(s, &mut locals).as_int(),
                        hi.eval(s, &mut locals).as_int(),
                    );
                    if l == h {
                        // One candidate: `successors_in` writes it in place.
                        for st in &mut acc {
                            st[*slot] = l;
                        }
                        continue;
                    }
                    let mut next = Vec::new();
                    for base in &acc {
                        let mut v = l;
                        while v <= h {
                            let mut st = base.clone();
                            st[*slot] = v;
                            next.push(st);
                            if v == h {
                                break;
                            }
                            v = v.wrapping_add(1);
                        }
                    }
                    acc = next;
                }
            }
        }
        acc
    }
}

/// A boolean expression resolved against one [`Compiled`] model.
pub(crate) struct CompiledExpr(Node);

impl CompiledExpr {
    /// The expression's value at `s` (`Expr::eval` over `eval_env(s)`).
    // Skip: drives the evaluator above.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn eval(&self, s: &[i64]) -> Value {
        self.0.eval(s, &mut Vec::new())
    }
}
