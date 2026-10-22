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

use std::cell::Cell;
use std::collections::BTreeMap;

use crate::derive::{Expr, Model, Value};

/// A panic `eval` raises where it reads an expression the interpreter cannot
/// evaluate, raised here with the same message at the same read.
#[derive(Debug, Clone, Copy)]
enum Fault {
    /// An identifier nothing binds.
    Unbound(&'static str),
    /// A function-valued expression in a scalar position.
    FnValued,
    /// A `lo..hi` set in a scalar position.
    SetValued,
}

impl Fault {
    fn raise(self) -> ! {
        match self {
            Fault::Unbound(n) => panic!("unbound identifier `{n}` in model evaluation"),
            Fault::FnValued => panic!(
                "function-valued Expr is TLA+-generation only (Tier-0 ty-checked, not interpreter-evaluable)"
            ),
            Fault::SetValued => panic!(
                "InRange (`lo..hi`) is set-valued; it is meaningful only as a nondeterministic \
                 update RHS — fire the action via `Model::successors`, not scalar `eval`"
            ),
        }
    }
}

/// An integer position (an update, a bound, an operand), resolved: what `eval`
/// computes there before its `as_int`, as an `i64`.
#[derive(Debug)]
enum Num {
    Atom(Atom),
    /// `Add`/`Sub` of two atoms, wrapping as `eval`'s: one step where the tree takes three.
    Arith(Arith, Atom, Atom),
    Add(Box<Num>, Box<Num>),
    Sub(Box<Num>, Box<Num>),
    If(Box<Test>, Box<Num>, Box<Num>),
    /// A boolean where an integer is read: evaluated, then `as_int`'s type error.
    Bool(Box<Test>),
    Fault(Fault),
}

/// A boolean position (a guard, an invariant, an operand), resolved: what
/// `eval` computes there before its `as_bool`, as a `bool`.
#[derive(Debug)]
enum Test {
    Const(bool),
    /// `Gt`/`Le`/`Eq`/`Neq` of two atoms.
    Cmp(Cmp, Atom, Atom),
    /// A run of two or more such comparisons inside one `/\`, all of which hold.
    Cmps(Box<[(Cmp, Atom, Atom)]>),
    /// A comparison of operands that are not both atoms.
    CmpOf(Cmp, Box<Num>, Box<Num>),
    /// A `/\` chain, its operands in order, stopping at the first false.
    And(Box<[Test]>),
    /// A `\/` chain, its operands in order, stopping at the first true.
    Or(Box<[Test]>),
    Iff(Box<Test>, Box<Test>),
    If(Box<Test>, Box<Test>, Box<Test>),
    /// `\A`: bounds, then the body with its index at local depth `.0`.
    Forall(usize, Box<Num>, Box<Num>, Box<Test>),
    /// An integer where a boolean is read: evaluated, then `as_bool`'s type error.
    Int(Box<Num>),
    Fault(Fault),
}

/// A position read whole (a liveness goal), whose type `eval`'s `Value` decides
/// per evaluation.
#[derive(Debug)]
enum Whole {
    Num(Num),
    Test(Test),
    If(Test, Box<Whole>, Box<Whole>),
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

impl Num {
    /// [`Num::eval`], with an atom and an operation over two atoms read in place rather than
    /// through a call.
    #[inline(always)]
    fn value(&self, s: &[i64], locals: &mut [i64]) -> i64 {
        match self {
            Num::Atom(a) => a.read(s, locals),
            Num::Arith(op, a, b) => op.apply(a.read(s, locals), b.read(s, locals)),
            n => n.eval(s, locals),
        }
    }

    // Skip: a recursive evaluator over a boxed tree whose panics are `eval`'s, message for
    // message.
    #[cfg_attr(trust_verify, trust::skip)]
    fn eval(&self, s: &[i64], locals: &mut [i64]) -> i64 {
        match self {
            Num::Atom(a) => a.read(s, locals),
            Num::Arith(op, a, b) => op.apply(a.read(s, locals), b.read(s, locals)),
            Num::Add(a, b) => a.value(s, locals).wrapping_add(b.value(s, locals)),
            Num::Sub(a, b) => a.value(s, locals).wrapping_sub(b.value(s, locals)),
            Num::If(c, a, b) => {
                if c.check(s, locals) {
                    a.value(s, locals)
                } else {
                    b.value(s, locals)
                }
            }
            Num::Bool(t) => Value::Bool(t.check(s, locals)).as_int(),
            Num::Fault(f) => f.raise(),
        }
    }
}

impl Test {
    /// [`Test::eval`], with a constant and atom comparisons read in place rather than through
    /// a call.
    #[inline(always)]
    fn check(&self, s: &[i64], locals: &mut [i64]) -> bool {
        match self {
            Test::Const(b) => *b,
            Test::Cmp(op, a, b) => op.holds(a.read(s, locals), b.read(s, locals)),
            Test::Cmps(run) => {
                for (op, a, b) in run {
                    if !op.holds(a.read(s, locals), b.read(s, locals)) {
                        return false;
                    }
                }
                true
            }
            t => t.eval(s, locals),
        }
    }

    // Skip: same class as `Num::eval`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn eval(&self, s: &[i64], locals: &mut [i64]) -> bool {
        match self {
            Test::Const(b) => *b,
            Test::Cmp(op, a, b) => op.holds(a.read(s, locals), b.read(s, locals)),
            Test::Cmps(run) => {
                for (op, a, b) in run {
                    if !op.holds(a.read(s, locals), b.read(s, locals)) {
                        return false;
                    }
                }
                true
            }
            Test::CmpOf(op, a, b) => op.holds(a.value(s, locals), b.value(s, locals)),
            Test::And(all) => {
                for t in all {
                    if !t.check(s, locals) {
                        return false;
                    }
                }
                true
            }
            Test::Or(any) => {
                for t in any {
                    if t.check(s, locals) {
                        return true;
                    }
                }
                false
            }
            Test::Iff(a, b) => a.check(s, locals) == b.check(s, locals),
            Test::If(c, a, b) => {
                if c.check(s, locals) {
                    a.check(s, locals)
                } else {
                    b.check(s, locals)
                }
            }
            Test::Forall(depth, lo, hi, body) => {
                let (l, h) = (lo.value(s, locals), hi.value(s, locals));
                // The index's slot: every enclosing `\A` holds the ones below it, and
                // nothing reads a slot deeper than the `\A` it is in.
                for n in l..=h {
                    locals[*depth] = n;
                    if !body.check(s, locals) {
                        return false;
                    }
                }
                true
            }
            Test::Int(n) => Value::Int(n.value(s, locals)).as_bool(),
            Test::Fault(f) => f.raise(),
        }
    }
}

impl Whole {
    // Skip: same class as `Num::eval`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn eval(&self, s: &[i64], locals: &mut [i64]) -> Value {
        match self {
            Whole::Num(n) => Value::Int(n.eval(s, locals)),
            Whole::Test(t) => Value::Bool(t.eval(s, locals)),
            Whole::If(c, a, b) => {
                if c.eval(s, locals) {
                    a.eval(s, locals)
                } else {
                    b.eval(s, locals)
                }
            }
        }
    }
}

/// How many `\A` index slots an evaluation holds on the stack; a model nesting deeper takes
/// them from the heap.
const STACK_SLOTS: usize = 8;

/// `f` given `slots` zeroed `\A` index slots: none for a model with no `\A`, on the stack
/// for every other but one nesting `\A` deeper than [`STACK_SLOTS`].
// Skip: a stack buffer or a heap one, handed to the caller's closure. Spec-model machinery.
#[cfg_attr(trust_verify, trust::skip)]
fn with_slots<R>(slots: usize, f: impl FnOnce(&mut [i64]) -> R) -> R {
    if slots == 0 {
        f(&mut [])
    } else if slots <= STACK_SLOTS {
        f(&mut [0; STACK_SLOTS])
    } else {
        f(&mut vec![0; slots])
    }
}

/// One update's right-hand side.
#[derive(Debug)]
enum Rhs {
    /// `var' = e`.
    Det(Num),
    /// `var' \in lo..hi`.
    Range(Num, Num),
}

#[derive(Debug)]
struct CAction {
    guard: Option<Test>,
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
    invariants: Vec<Test>,
    /// The `\A` index slots the deepest of its expressions takes.
    slots: usize,
}

/// Name resolution for one model: the scope a compiled expression reads.
struct Scope<'a> {
    slots: &'a BTreeMap<&'static str, usize>,
    consts: &'a BTreeMap<&'static str, i64>,
    /// One past the deepest `\A` index compiled so far: the slots an evaluation needs.
    depth: Cell<usize>,
}

impl Scope<'_> {
    /// `e` read as an integer.
    // Skip: recursive tree construction (Box allocation, map lookups).
    #[cfg_attr(trust_verify, trust::skip)]
    fn num(&self, e: &Expr, bound: &mut Vec<&'static str>) -> Num {
        // An operation over two atoms is one node, and over two constants its value: neither
        // reads anything that can panic, so `eval`'s order and laziness have nothing to keep.
        let arith = |op: Arith,
                     a: &Expr,
                     b: &Expr,
                     bound: &mut Vec<&'static str>,
                     tree: fn(Box<Num>, Box<Num>) -> Num| {
            match (self.num(a, bound), self.num(b, bound)) {
                (Num::Atom(Atom::Int(x)), Num::Atom(Atom::Int(y))) => {
                    Num::Atom(Atom::Int(op.apply(x, y)))
                }
                (Num::Atom(x), Num::Atom(y)) => Num::Arith(op, x, y),
                (a, b) => tree(Box::new(a), Box::new(b)),
            }
        };
        match e {
            Expr::Int(i) => Num::Atom(Atom::Int(*i)),
            Expr::Var(n) | Expr::ConstRef(n) => {
                if let Some(depth) = bound.iter().rposition(|b| b == n) {
                    Num::Atom(Atom::Local(depth))
                } else if let Some(&slot) = self.slots.get(n) {
                    Num::Atom(Atom::Slot(slot))
                } else if let Some(&v) = self.consts.get(n) {
                    Num::Atom(Atom::Int(v))
                } else {
                    Num::Fault(Fault::Unbound(n))
                }
            }
            Expr::Add(a, b) => arith(Arith::Add, a, b, bound, Num::Add),
            Expr::Sub(a, b) => arith(Arith::Sub, a, b, bound, Num::Sub),
            Expr::If(c, a, b) => Num::If(
                Box::new(self.test(c, bound)),
                Box::new(self.num(a, bound)),
                Box::new(self.num(b, bound)),
            ),
            Expr::Gt(..)
            | Expr::Le(..)
            | Expr::Eq(..)
            | Expr::Neq(..)
            | Expr::Or(..)
            | Expr::And(..)
            | Expr::Iff(..)
            | Expr::Forall(..)
            | Expr::Bool(_) => Num::Bool(Box::new(self.test(e, bound))),
            Expr::FnAccess(..) | Expr::Except(..) | Expr::Comprehension(..) => {
                Num::Fault(Fault::FnValued)
            }
            Expr::InRange(..) => Num::Fault(Fault::SetValued),
        }
    }

    /// `e` read as a boolean.
    // Skip: same class as `num`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn test(&self, e: &Expr, bound: &mut Vec<&'static str>) -> Test {
        let cmp = |op: Cmp, a: &Expr, b: &Expr, bound: &mut Vec<&'static str>| match (
            self.num(a, bound),
            self.num(b, bound),
        ) {
            (Num::Atom(Atom::Int(x)), Num::Atom(Atom::Int(y))) => Test::Const(op.holds(x, y)),
            (Num::Atom(x), Num::Atom(y)) => Test::Cmp(op, x, y),
            (a, b) => Test::CmpOf(op, Box::new(a), Box::new(b)),
        };
        match e {
            Expr::Bool(b) => Test::Const(*b),
            Expr::Gt(a, b) => cmp(Cmp::Gt, a, b, bound),
            Expr::Le(a, b) => cmp(Cmp::Le, a, b, bound),
            Expr::Eq(a, b) => cmp(Cmp::Eq, a, b, bound),
            Expr::Neq(a, b) => cmp(Cmp::Neq, a, b, bound),
            Expr::And(..) => {
                let mut all = Vec::new();
                self.chain(e, true, bound, &mut all);
                chained(all, true)
            }
            Expr::Or(..) => {
                let mut any = Vec::new();
                self.chain(e, false, bound, &mut any);
                chained(any, false)
            }
            Expr::Iff(a, b) => {
                Test::Iff(Box::new(self.test(a, bound)), Box::new(self.test(b, bound)))
            }
            Expr::If(c, a, b) => match self.test(c, bound) {
                // A constant condition reads one arm only, as `eval` does.
                Test::Const(true) => self.test(a, bound),
                Test::Const(false) => self.test(b, bound),
                c => Test::If(
                    Box::new(c),
                    Box::new(self.test(a, bound)),
                    Box::new(self.test(b, bound)),
                ),
            },
            Expr::Forall(idx, lo, hi, body) => {
                // The bounds read the enclosing scope; only the body sees `idx`.
                let lo = Box::new(self.num(lo, bound));
                let hi = Box::new(self.num(hi, bound));
                let depth = bound.len();
                self.depth.set(self.depth.get().max(depth + 1));
                bound.push(idx);
                let body = Box::new(self.test(body, bound));
                bound.pop();
                Test::Forall(depth, lo, hi, body)
            }
            Expr::Int(_) | Expr::Var(_) | Expr::ConstRef(_) | Expr::Add(..) | Expr::Sub(..) => {
                Test::Int(Box::new(self.num(e, bound)))
            }
            Expr::FnAccess(..) | Expr::Except(..) | Expr::Comprehension(..) => {
                Test::Fault(Fault::FnValued)
            }
            Expr::InRange(..) => Test::Fault(Fault::SetValued),
        }
    }

    /// The operands of the `/\` (`all`) or `\/` chain at `e`, flattened in
    /// evaluation order onto `out`.
    // Skip: same class as `num`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn chain(&self, e: &Expr, all: bool, bound: &mut Vec<&'static str>, out: &mut Vec<Test>) {
        match (e, all) {
            (Expr::And(a, b), true) | (Expr::Or(a, b), false) => {
                self.chain(a, all, bound, out);
                self.chain(b, all, bound, out);
            }
            _ => out.push(self.test(e, bound)),
        }
    }

    /// `e` read whole.
    // Skip: same class as `num`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn whole(&self, e: &Expr, bound: &mut Vec<&'static str>) -> Whole {
        match e {
            Expr::If(c, a, b) => Whole::If(
                self.test(c, bound),
                Box::new(self.whole(a, bound)),
                Box::new(self.whole(b, bound)),
            ),
            Expr::Gt(..)
            | Expr::Le(..)
            | Expr::Eq(..)
            | Expr::Neq(..)
            | Expr::Or(..)
            | Expr::And(..)
            | Expr::Iff(..)
            | Expr::Forall(..)
            | Expr::Bool(_) => Whole::Test(self.test(e, bound)),
            _ => Whole::Num(self.num(e, bound)),
        }
    }
}

/// A flattened `/\` (`all`) or `\/` chain as one test, in `eval`'s order and
/// with its short circuit. An operand that cannot stop the chain (`TRUE` in a
/// `/\`) is dropped; one that always stops it is kept as the chain's last, since
/// nothing after it is read; a run of atom comparisons in a `/\` is one step.
// Skip: Vec bookkeeping over compiled operands. Spec-model machinery.
#[cfg_attr(trust_verify, trust::skip)]
fn chained(operands: Vec<Test>, all: bool) -> Test {
    let mut kept: Vec<Test> = Vec::new();
    for t in operands {
        match t {
            Test::Const(b) if b == all => {}
            Test::Const(_) => {
                kept.push(t);
                break;
            }
            t => kept.push(t),
        }
    }
    if all {
        let mut runs: Vec<Test> = Vec::new();
        for t in kept {
            match (runs.last_mut(), t) {
                (Some(Test::Cmp(op, a, b)), Test::Cmp(op2, a2, b2)) => {
                    let first = (*op, *a, *b);
                    *runs.last_mut().expect("just matched") =
                        Test::Cmps(Box::new([first, (op2, a2, b2)]));
                }
                (Some(Test::Cmps(run)), Test::Cmp(op2, a2, b2)) => {
                    let mut longer = run.to_vec();
                    longer.push((op2, a2, b2));
                    *run = longer.into_boxed_slice();
                }
                (_, t) => runs.push(t),
            }
        }
        kept = runs;
    }
    match kept.len() {
        0 => Test::Const(all),
        1 => kept.pop().expect("one operand"),
        _ if all => Test::And(kept.into_boxed_slice()),
        _ => Test::Or(kept.into_boxed_slice()),
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
            depth: Cell::new(0),
        };
        let mut bound = Vec::new();
        let mut actions = Vec::with_capacity(m.actions.len());
        for a in &m.actions {
            let mut updates = Vec::with_capacity(a.updates.len());
            for u in &a.updates {
                let slot = *slots.get(u.var)?;
                let rhs = match &u.expr {
                    Expr::InRange(lo, hi) => {
                        Rhs::Range(scope.num(lo, &mut bound), scope.num(hi, &mut bound))
                    }
                    e => Rhs::Det(scope.num(e, &mut bound)),
                };
                updates.push((slot, rhs));
            }
            actions.push(CAction {
                guard: a.guard.as_ref().map(|g| scope.test(g, &mut bound)),
                updates,
            });
        }
        let invariants = m
            .invariants
            .iter()
            .map(|i| scope.test(&i.expr, &mut bound))
            .collect();
        Some(Self {
            names,
            init,
            actions,
            invariants,
            slots: scope.depth.get(),
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
            depth: Cell::new(0),
        };
        let whole = scope.whole(e, &mut Vec::new());
        CompiledExpr(whole, scope.depth.get())
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
        with_slots(self.slots, |locals| self.invariants[i].check(s, locals))
    }

    /// How many values a state has: the length of every state slice.
    pub(crate) fn width(&self) -> usize {
        self.names.len()
    }

    /// `Model::successors_in` for action `a` at `s`, in the same order.
    // Skip: collects `successors_into`'s rows, one Vec per successor.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn successors(&self, a: usize, s: &[i64]) -> Vec<Vec<i64>> {
        let mut rows = Vec::new();
        let made = self.successors_into(a, s, &mut rows);
        (0..made)
            .map(|k| rows[k * s.len()..(k + 1) * s.len()].to_vec())
            .collect()
    }

    /// [`Compiled::successors`] written into `rows` (replacing what it held) one after
    /// another, each `s.len()` values long; returns how many. A walk passes the same buffer
    /// for every step, so a step allocates nothing once it has grown.
    // Skip: the cartesian product's Vec bookkeeping, same class as
    // `successors_in`, whose algorithm this is.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn successors_into(&self, a: usize, s: &[i64], rows: &mut Vec<i64>) -> usize {
        with_slots(self.slots, |locals| {
            self.successors_with(a, s, rows, locals)
        })
    }

    /// [`Compiled::successors_into`] with its `\A` index slots.
    // Skip: same class as `successors_into`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn successors_with(
        &self,
        a: usize,
        s: &[i64],
        rows: &mut Vec<i64>,
        locals: &mut [i64],
    ) -> usize {
        let act = &self.actions[a];
        rows.clear();
        if act.guard.as_ref().is_some_and(|g| !g.check(s, locals)) {
            return 0;
        }
        // Every right-hand side reads the pre-state `s`, so evaluating each just before it is
        // applied matches evaluating all first (same values, order and panics). Each is
        // evaluated even once the product is empty, as `successors_in` does.
        let width = s.len();
        rows.extend_from_slice(s);
        let mut made = 1;
        for (slot, rhs) in &act.updates {
            match rhs {
                Rhs::Det(e) => {
                    let v = e.value(s, locals);
                    write_slot(rows, width, *slot, v);
                }
                Rhs::Range(lo, hi) => {
                    let (l, h) = (lo.value(s, locals), hi.value(s, locals));
                    if l == h {
                        // One candidate: `successors_in` writes it in place.
                        write_slot(rows, width, *slot, l);
                        continue;
                    }
                    // Each row so far, once per candidate in ascending order, appended
                    // after the rows so far; then those are dropped.
                    let before = made * width;
                    made = 0;
                    let mut base = 0;
                    while base < before {
                        let mut v = l;
                        while v <= h {
                            rows.extend_from_within(base..base + width);
                            let at = rows.len() - width + slot;
                            rows[at] = v;
                            made += 1;
                            if v == h {
                                break;
                            }
                            v = v.wrapping_add(1);
                        }
                        base += width;
                    }
                    rows.drain(..before);
                }
            }
        }
        made
    }
}

/// Write `v` at `slot` of every `width`-long row in `rows`.
fn write_slot(rows: &mut [i64], width: usize, slot: usize, v: i64) {
    let mut at = slot;
    while at < rows.len() {
        rows[at] = v;
        at += width;
    }
}

/// A boolean expression resolved against one [`Compiled`] model.
pub(crate) struct CompiledExpr(Whole, usize);

impl CompiledExpr {
    /// The expression's value at `s` (`Expr::eval` over `eval_env(s)`).
    // Skip: drives the evaluator above.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn eval(&self, s: &[i64]) -> Value {
        with_slots(self.1, |locals| self.0.eval(s, locals))
    }
}
