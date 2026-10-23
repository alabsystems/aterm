// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Emits registered `ty_model!` models, `islands/`, and `progress/` obligations as Trust `ScalarModel` and `ScalarProgress` files.
//! Splits `InRange` updates into deterministic actions, transposes subtractions out of comparisons, and rejects models that underflow on reachable states.

use std::fmt::Write as _;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Action, Expr, Liveness, Model, Update, Value};
use aterm_spec::interp;

const NS: &str = "Trust.Temporal.FiniteModel";

fn nat(value: i64, what: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("negative {what} {value}"))
}

/// Output spelling: standalone `ScalarModel` constructors or `Trust.Protocol` operators.
#[derive(Clone, Copy)]
enum Style {
    Constructors,
    Protocol,
}

fn arg(text: String) -> String {
    if text.starts_with('(') {
        text
    } else {
        format!("({text})")
    }
}

fn expr(e: &Expr, style: Style) -> Result<String, String> {
    let two = |name: &str, op: &str, a: &Expr, b: &Expr| -> Result<String, String> {
        Ok(match style {
            Style::Constructors => format!(
                "(ScalarExpr.{name} {} {})",
                expr(a, style)?,
                expr(b, style)?
            ),
            Style::Protocol if op.is_empty() => format!(
                "({NS}.ScalarExpr.{name} {} {})",
                arg(expr(a, style)?),
                arg(expr(b, style)?)
            ),
            Style::Protocol => format!("({} {op} {})", expr(a, style)?, expr(b, style)?),
        })
    };
    let protocol = matches!(style, Style::Protocol);
    match e {
        Expr::Int(i) if protocol => Ok(format!("n {}", nat(*i, "literal")?)),
        Expr::Var(name) if protocol => Ok(format!("v \"{name}\"")),
        Expr::ConstRef(name) if protocol => Ok(format!("k \"{name}\"")),
        Expr::Int(i) => Ok(format!("(ScalarExpr.int {})", nat(*i, "literal")?)),
        Expr::Var(name) => Ok(format!("(ScalarExpr.var \"{name}\")")),
        Expr::ConstRef(name) => Ok(format!("(ScalarExpr.constRef \"{name}\")")),
        Expr::Add(a, b) => two("add", "⊞", a, b),
        Expr::Sub(a, b) => two("sub", "⊟", a, b),
        Expr::Gt(a, b) => two("gt", "≻", a, b),
        Expr::Le(a, b) => two("le", "≼", a, b),
        Expr::Eq(a, b) => two("eq", "≐", a, b),
        Expr::Neq(a, b) => two("neq", "≢", a, b),
        Expr::Or(a, b) => two("or", "⋎", a, b),
        Expr::And(a, b) => two("and", "⋏", a, b),
        Expr::Iff(a, b) => two("iff", "", a, b),
        Expr::If(c, a, b) if protocol => Ok(format!(
            "(cond {} {} {})",
            arg(expr(c, style)?),
            arg(expr(a, style)?),
            arg(expr(b, style)?)
        )),
        Expr::If(c, a, b) => Ok(format!(
            "(ScalarExpr.ite {} {} {})",
            expr(c, style)?,
            expr(a, style)?,
            expr(b, style)?
        )),
        Expr::Bool(b) if protocol => Ok(format!("({NS}.ScalarExpr.bool Bool.{b})")),
        Expr::Bool(b) => Ok(format!("(ScalarExpr.bool Bool.{b})")),
        Expr::Forall(..) => Err("a quantifier (Forall)".to_owned()),
        Expr::FnAccess(..) | Expr::Except(..) | Expr::Comprehension(..) => {
            Err("function-valued state".to_owned())
        }
        Expr::InRange(..) => Err("a nondeterministic update (InRange)".to_owned()),
    }
}

/// Partition `e` into positive (`plus`) and negated (`minus`) additive atoms.
fn linear(e: &Expr, positive: bool, plus: &mut Vec<Expr>, minus: &mut Vec<Expr>) {
    match e {
        Expr::Add(a, b) => {
            linear(a, positive, plus, minus);
            linear(b, positive, plus, minus);
        }
        Expr::Sub(a, b) => {
            linear(a, positive, plus, minus);
            linear(b, !positive, plus, minus);
        }
        atom => {
            if positive {
                plus.push(transpose(atom))
            } else {
                minus.push(transpose(atom))
            }
        }
    }
}

fn sum(atoms: Vec<Expr>) -> Expr {
    atoms
        .into_iter()
        .reduce(|total, atom| Expr::Add(Box::new(total), Box::new(atom)))
        .unwrap_or(Expr::Int(0))
}

/// Transpose subtrahends across comparisons so no subtraction sits directly under a comparison.
fn transpose(e: &Expr) -> Expr {
    let compare = |build: fn(Box<Expr>, Box<Expr>) -> Expr, a: &Expr, b: &Expr| {
        let (mut left, mut right) = (Vec::new(), Vec::new());
        linear(a, true, &mut left, &mut right);
        linear(b, true, &mut right, &mut left);
        build(Box::new(sum(left)), Box::new(sum(right)))
    };
    let both = |build: fn(Box<Expr>, Box<Expr>) -> Expr, a: &Expr, b: &Expr| {
        build(Box::new(transpose(a)), Box::new(transpose(b)))
    };
    match e {
        Expr::Gt(a, b) => compare(Expr::Gt, a, b),
        Expr::Le(a, b) => compare(Expr::Le, a, b),
        Expr::Eq(a, b) => compare(Expr::Eq, a, b),
        Expr::Neq(a, b) => compare(Expr::Neq, a, b),
        Expr::Add(a, b) => both(Expr::Add, a, b),
        Expr::Sub(a, b) => both(Expr::Sub, a, b),
        Expr::Or(a, b) => both(Expr::Or, a, b),
        Expr::And(a, b) => both(Expr::And, a, b),
        Expr::Iff(a, b) => both(Expr::Iff, a, b),
        Expr::If(c, a, b) => Expr::If(
            Box::new(transpose(c)),
            Box::new(transpose(a)),
            Box::new(transpose(b)),
        ),
        Expr::InRange(a, b) => both(Expr::InRange, a, b),
        other => other.clone(),
    }
}

/// Rewrite identifiers in `e` as [`Expr::Var`] when declared in `vars` and [`Expr::ConstRef`] otherwise.
fn resolved(e: &Expr, vars: &BTreeSet<&'static str>) -> Expr {
    let both = |build: fn(Box<Expr>, Box<Expr>) -> Expr, a: &Expr, b: &Expr| {
        build(Box::new(resolved(a, vars)), Box::new(resolved(b, vars)))
    };
    match e {
        Expr::Var(name) | Expr::ConstRef(name) => {
            if vars.contains(name) {
                Expr::Var(name)
            } else {
                Expr::ConstRef(name)
            }
        }
        Expr::Add(a, b) => both(Expr::Add, a, b),
        Expr::Sub(a, b) => both(Expr::Sub, a, b),
        Expr::Gt(a, b) => both(Expr::Gt, a, b),
        Expr::Le(a, b) => both(Expr::Le, a, b),
        Expr::Eq(a, b) => both(Expr::Eq, a, b),
        Expr::Neq(a, b) => both(Expr::Neq, a, b),
        Expr::Or(a, b) => both(Expr::Or, a, b),
        Expr::And(a, b) => both(Expr::And, a, b),
        Expr::Iff(a, b) => both(Expr::Iff, a, b),
        Expr::InRange(a, b) => both(Expr::InRange, a, b),
        Expr::If(c, a, b) => Expr::If(
            Box::new(resolved(c, vars)),
            Box::new(resolved(a, vars)),
            Box::new(resolved(b, vars)),
        ),
        other => other.clone(),
    }
}

fn variables(m: &Model) -> BTreeSet<&'static str> {
    m.vars.iter().map(|var| var.name).collect()
}

fn scalar_form(e: &Expr, vars: &BTreeSet<&'static str>) -> Expr {
    transpose(&resolved(e, vars))
}

fn transposed(m: &Model) -> Model {
    let vars = variables(m);
    let transpose = |e: &Expr| scalar_form(e, &vars);
    let mut m = m.clone();
    for action in &mut m.actions {
        action.guard = action.guard.as_ref().map(&transpose);
        for update in &mut action.updates {
            update.expr = transpose(&update.expr);
        }
    }
    for invariant in &mut m.invariants {
        invariant.expr = transpose(&invariant.expr);
    }
    m
}

fn int(e: &Expr, env: &BTreeMap<&'static str, i64>) -> i64 {
    match e.eval(env) {
        Value::Int(value) => value,
        Value::Bool(value) => i64::from(value),
    }
}

fn truth(e: &Expr, env: &BTreeMap<&'static str, i64>) -> bool {
    match e.eval(env) {
        Value::Int(value) => value != 0,
        Value::Bool(value) => value,
    }
}

/// Return whether evaluating `e` in `env` performs a negative subtraction on a taken branch.
fn underflows(e: &Expr, env: &BTreeMap<&'static str, i64>) -> bool {
    match e {
        Expr::Sub(a, b) => underflows(a, env) || underflows(b, env) || int(a, env) < int(b, env),
        Expr::Add(a, b)
        | Expr::InRange(a, b)
        | Expr::Gt(a, b)
        | Expr::Le(a, b)
        | Expr::Eq(a, b)
        | Expr::Neq(a, b)
        | Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Iff(a, b) => underflows(a, env) || underflows(b, env),
        Expr::If(c, a, b) => {
            underflows(c, env) || underflows(if truth(c, env) { a } else { b }, env)
        }
        _ => false,
    }
}

/// Collect reachable states of `m` under integer semantics, returning `None` past `limit`.
fn reachable(m: &Model, limit: usize) -> Option<Vec<BTreeMap<&'static str, i64>>> {
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut order = Vec::new();
    let mut queue = VecDeque::new();
    let init = m.init_state();
    seen.insert(init.values().copied().collect());
    queue.push_back(init);
    while let Some(state) = queue.pop_front() {
        let env = m.eval_env(&state);
        for action in &m.actions {
            for next in m.successors_in(action, &env, &state) {
                if seen.insert(next.values().copied().collect()) {
                    if seen.len() > limit {
                        return None;
                    }
                    queue.push_back(next);
                }
            }
        }
        order.push(state);
    }
    Some(order)
}

/// Split every [`Expr::InRange`] update in `m` into one deterministic action per choice.
fn determinized(m: &Model) -> Result<Model, String> {
    const MAX_CHOICES: usize = 4096;
    let ranged = |action: &Action| {
        action
            .updates
            .iter()
            .any(|update| matches!(update.expr, Expr::InRange(..)))
    };
    if !m.actions.iter().any(ranged) {
        return Ok(m.clone());
    }
    let states = reachable(m, 200_000).ok_or("more states than the range survey explores")?;
    let mut actions = Vec::new();
    for action in &m.actions {
        if !ranged(action) {
            actions.push(action.clone());
            continue;
        }
        let mut widest: Vec<Option<i64>> = vec![None; action.updates.len()];
        for state in &states {
            let env = m.eval_env(state);
            if action
                .guard
                .as_ref()
                .is_some_and(|guard| !truth(guard, &env))
            {
                continue;
            }
            for (slot, update) in action.updates.iter().enumerate() {
                if let Expr::InRange(lo, hi) = &update.expr {
                    let span = int(hi, &env) - int(lo, &env);
                    widest[slot] = Some(widest[slot].map_or(span, |widest| widest.max(span)));
                }
            }
        }
        let mut combinations: Vec<Vec<i64>> = vec![Vec::new()];
        for (slot, update) in action.updates.iter().enumerate() {
            let choices: Vec<i64> = match (&update.expr, widest[slot]) {
                (Expr::InRange(..), Some(span)) => (0..=span).collect(),
                (Expr::InRange(..), None) => Vec::new(),
                _ => vec![0],
            };
            combinations = combinations
                .iter()
                .flat_map(|chosen| {
                    choices.iter().map(move |choice| {
                        let mut chosen = chosen.clone();
                        chosen.push(*choice);
                        chosen
                    })
                })
                .collect();
            if combinations.len() > MAX_CHOICES {
                return Err(format!(
                    "`{}` has more than {MAX_CHOICES} choices",
                    action.name
                ));
            }
        }
        for chosen in combinations {
            let mut guard = action.guard.clone();
            let mut updates = Vec::new();
            let mut label = String::new();
            for (update, choice) in action.updates.iter().zip(&chosen) {
                let Expr::InRange(lo, hi) = &update.expr else {
                    updates.push(update.clone());
                    continue;
                };
                let value = Expr::Add(lo.clone(), Box::new(Expr::Int(*choice)));
                let within = Expr::Le(Box::new(value.clone()), hi.clone());
                guard = Some(match guard {
                    Some(guard) => Expr::And(Box::new(guard), Box::new(within)),
                    None => within,
                });
                updates.push(Update {
                    var: update.var,
                    expr: value,
                });
                let _ = write!(label, "_{choice}");
            }
            let name: &'static str = Box::leak(format!("{}{label}", action.name).into_boxed_str());
            actions.push(Action {
                name,
                guard,
                updates,
            });
        }
    }
    let mut split = m.clone();
    split.actions = actions;
    Ok(split)
}

/// Verify that no subtraction underflows on any reachable state of `m`.
fn natural_on_every_reachable_state(m: &Model) -> Result<(), String> {
    const MAX_STATES: usize = 200_000;
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut queue = VecDeque::new();
    let init = m.init_state();
    seen.insert(init.values().copied().collect());
    queue.push_back(init);
    while let Some(state) = queue.pop_front() {
        let env = m.eval_env(&state);
        for invariant in &m.invariants {
            if underflows(&invariant.expr, &env) {
                return Err(format!("invariant `{}`", invariant.name));
            }
        }
        for action in &m.actions {
            if action
                .guard
                .as_ref()
                .is_some_and(|guard| underflows(guard, &env))
            {
                return Err(format!("the guard of `{}`", action.name));
            }
            if action
                .guard
                .as_ref()
                .is_some_and(|guard| !truth(guard, &env))
            {
                continue;
            }
            if action
                .updates
                .iter()
                .any(|update| underflows(&update.expr, &env))
            {
                return Err(format!("an update of `{}`", action.name));
            }
            for next in m.successors_in(action, &env, &state) {
                if seen.insert(next.values().copied().collect()) {
                    if seen.len() > MAX_STATES {
                        return Err("more states than this check explores".to_owned());
                    }
                    queue.push_back(next);
                }
            }
        }
    }
    Ok(())
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "[]".to_owned()
    } else {
        format!("[{}]", items.join(",\n     "))
    }
}

fn scalar_model(m: &Model, def: &str, style: Style) -> Result<String, String> {
    if !m.fn_vars.is_empty() {
        return Err("function-valued state".to_owned());
    }
    let protocol = matches!(style, Style::Protocol);
    let mut consts = Vec::new();
    for (name, value) in &m.consts {
        let value = nat(*value, "constant")?;
        consts.push(if protocol {
            format!("param \"{name}\" {value}")
        } else {
            format!("Constant.mk \"{name}\" {value}")
        });
    }
    let mut vars = Vec::new();
    for var in &m.vars {
        let init = nat(var.init, "initial value")?;
        vars.push(if protocol {
            format!("var \"{}\" {init}", var.name)
        } else {
            format!("StateVar.mk \"{}\" {init}", var.name)
        });
    }
    let mut actions = Vec::new();
    for action in &m.actions {
        let mut updates = Vec::new();
        for update in &action.updates {
            let value = expr(&update.expr, style)?;
            updates.push(if protocol {
                format!("\"{}\" ≔ {value}", update.var)
            } else {
                format!("Update.mk \"{}\" {value}", update.var)
            });
        }
        let updates = match style {
            Style::Protocol => format!("[{}]", updates.join(", ")),
            Style::Constructors => list(&updates),
        };
        actions.push(match (&action.guard, protocol) {
            (Some(g), true) => format!(
                "act \"{}\" {}\n       {updates}",
                action.name,
                arg(expr(g, style)?)
            ),
            (None, true) => format!("always \"{}\"\n       {updates}", action.name),
            (Some(g), false) => format!(
                "Action.mk \"{}\" (Guard.when {})\n       {updates}",
                action.name,
                expr(g, style)?
            ),
            (None, false) => format!(
                "Action.mk \"{}\" Guard.always\n       {updates}",
                action.name
            ),
        });
    }
    let mut invariants = Vec::new();
    for invariant in &m.invariants {
        let holds = expr(&invariant.expr, style)?;
        invariants.push(if protocol {
            format!("inv \"{}\" {}", invariant.name, arg(holds))
        } else {
            format!("Invariant.mk \"{}\" {holds}", invariant.name)
        });
    }
    let (ty, mk) = if protocol {
        (format!("{NS}.ScalarModel"), "machine")
    } else {
        ("ScalarModel".to_owned(), "ScalarModel.mk")
    };
    Ok(format!(
        "def {def} : {ty} :=\n  {mk} \"{}\"\n    {}\n    {}\n    {}\n    {}\n",
        m.name,
        list(&consts),
        list(&vars),
        list(&actions),
        list(&invariants)
    ))
}

fn names(actions: &[&'static str]) -> String {
    format!(
        "[{}]",
        actions
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Emit one liveness obligation as an island of `ScalarProgress` claims with its reachable-state count.
fn progress_island(index: usize, m: &Model, live: &Liveness) -> Result<(String, usize), String> {
    let faithful = |m: &Model| -> Result<Model, String> {
        let split = transposed(&determinized(m)?);
        natural_on_every_reachable_state(&split)
            .map_err(|place| format!("integer subtraction underflows in {place}"))?;
        for name in live.weak.iter().chain(&live.strong) {
            if !split.actions.iter().any(|action| action.name == *name) {
                return Err(format!("the fair action `{name}` is nondeterministic"));
            }
        }
        Ok(split)
    };
    let committed = interp::Space::of(&interp::with_buggy(m, 0));
    let design = faithful(m)?;
    let goal = arg(expr(
        &scalar_form(&live.goal, &variables(m)),
        Style::Protocol,
    )?);
    let claim = |name: &str, model: &str, weak: &[&'static str], strong: &[&'static str]| {
        format!(
            "def {name} : {NS}.ScalarProgress :=\n  progress {model} {goal}\n    {} {}\n",
            names(weak),
            names(strong)
        )
    };
    let mut text = scalar_model(&design, "design", Style::Protocol)?;
    text.push_str(&claim("holds", "design", &live.weak, &live.strong));
    for dropped in live.weak.iter().chain(&live.strong) {
        let keep = |actions: &[&'static str]| -> Vec<&'static str> {
            actions
                .iter()
                .copied()
                .filter(|name| name != dropped)
                .collect()
        };
        text.push_str(&claim(
            &format!("fails_without_{dropped}"),
            "design",
            &keep(&live.weak),
            &keep(&live.strong),
        ));
    }
    for mutant in &live.mutants {
        let mut isolated = interp::with_buggy(m, 1);
        isolated
            .actions
            .retain(|action| committed.fired().contains(action.name) || action.name == *mutant);
        let Ok(isolated) = faithful(&isolated) else {
            continue;
        };
        text.push_str(&scalar_model(
            &isolated,
            &format!("model_with_{mutant}"),
            Style::Protocol,
        )?);
        text.push_str(&claim(
            &format!("fails_mutant_{mutant}"),
            &format!("model_with_{mutant}"),
            &live.weak,
            &live.strong,
        ));
    }
    Ok((
        format!(
            "import Trust.Protocol\nnamespace AtermLive{index}\nopen Trust.Protocol\n{text}end \
             AtermLive{index}\n"
        ),
        committed.states(),
    ))
}

/// Run [`interp::bmc`], returning `None` if state-space exploration exceeds the interpreter's limit.
fn bounded_check(m: &Model) -> Option<Result<usize, String>> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result =
        std::panic::catch_unwind(|| interp::bmc(m).map_err(|(_, invariant)| invariant.to_owned()));
    std::panic::set_hook(hook);
    result.ok()
}

fn main() {
    let _domains = aterm_models::link_all();
    let out = std::env::args()
        .nth(1)
        .expect("usage: emit_clean_models <out-dir>");
    std::fs::create_dir_all(format!("{out}/islands")).expect("create the output directory");
    let mut manifest = String::from("index\tname\tstates\tmutants\tstatus\tother dials\n");
    for (index, model) in aterm_spec::xref::model_registry().iter().enumerate() {
        let original = model;
        let model = &match determinized(original) {
            Ok(split) => transposed(&split),
            Err(why) => {
                let _ = writeln!(
                    manifest,
                    "{index}\t{}\t-\t-\tnot emitted: {why}",
                    original.name
                );
                continue;
            }
        };
        let mut text = match scalar_model(model, "design", Style::Constructors) {
            Ok(text) => text,
            Err(why) => {
                let _ = writeln!(
                    manifest,
                    "{index}\t{}\t-\t-\tnot emitted: {why}",
                    model.name
                );
                continue;
            }
        };
        let states = match interp::bmc(model) {
            Ok(states) => states,
            Err((_, invariant)) => {
                let _ = writeln!(
                    manifest,
                    "{index}\t{}\t-\t-\tUNSAFE in aterm: {invariant}",
                    model.name
                );
                continue;
            }
        };
        assert_eq!(
            interp::bmc(original).ok(),
            Some(states),
            "{}: transposing changed the model",
            model.name
        );
        if let Err(place) = natural_on_every_reachable_state(model) {
            let _ = writeln!(
                manifest,
                "{index}\t{}\t{states}\t-\tnot emitted: integer subtraction underflows in {place}",
                model.name
            );
            continue;
        }
        let mut mutants: Vec<(i64, Model)> = Vec::new();
        let mut others: Vec<String> = Vec::new();
        for dial in original.mutant_dials() {
            let Ok(split) = determinized(&interp::with_buggy(original, dial)) else {
                others.push(format!("{dial}: not determinized"));
                continue;
            };
            let mutant = transposed(&split);
            match bounded_check(&mutant) {
                None => others.push(format!("{dial}: past the interpreter's limit")),
                Some(Ok(_)) => others.push(format!("{dial}: safe")),
                Some(Err(_)) if natural_on_every_reachable_state(&mutant).is_err() => {
                    others.push(format!("{dial}: subtraction underflows"));
                }
                Some(Err(_)) => mutants.push((dial, mutant)),
            }
        }
        let mut island = scalar_model(model, "design", Style::Protocol).expect("emitted above");
        let _ = write!(
            text,
            "theorem design_safe : safetyVerdict design {states} = CheckVerdict.ok := rfl\n\
             theorem design_states : List.length (reachRows design {states}) = {states} := rfl\n"
        );
        for (dial, mutant) in &mutants {
            let name = format!("mutant{dial}");
            island.push_str(&scalar_model(mutant, &name, Style::Protocol).expect("emitted above"));
            text.push_str(
                &scalar_model(mutant, &name, Style::Constructors).expect("emitted above"),
            );
            let _ = writeln!(
                text,
                "theorem {name}_caught : invariantBroken (safetyVerdict {name} 4096) = true := rfl"
            );
        }
        std::fs::write(
            format!("{out}/islands/{index:03}_{}.lean", model.name),
            format!(
                "import Trust.Protocol\nnamespace Aterm{index}\nopen Trust.Protocol\n{island}end \
                 Aterm{index}\n"
            ),
        )
        .expect("write the island");
        let file = format!("{out}/{index:03}_{}.lean", model.name);
        std::fs::write(
            &file,
            format!(
                "namespace Aterm{index}\nopen {NS}\n\
                 def invariantBroken (v : CheckVerdict) : Bool :=\n  \
                 @CheckVerdict.rec (fun _ => Bool) false false false (fun _ => false)\n    \
                 (fun _ _ => true) (fun _ _ _ => false) v\n{text}end Aterm{index}\n"
            ),
        )
        .expect("write the model");
        let emitted: Vec<String> = mutants.iter().map(|(dial, _)| dial.to_string()).collect();
        let _ = writeln!(
            manifest,
            "{index}\t{}\t{states}\t{}\temitted\t{}",
            model.name,
            if emitted.is_empty() {
                "-".to_owned()
            } else {
                emitted.join(",")
            },
            if others.is_empty() {
                "-".to_owned()
            } else {
                others.join("; ")
            },
        );
    }
    std::fs::write(format!("{out}/manifest.tsv"), manifest).expect("write the manifest");

    std::fs::create_dir_all(format!("{out}/progress")).expect("create the output directory");
    let mut progress = String::from("index\tmodel\tname\tstates\tstatus\n");
    for (index, (model, live)) in aterm_spec::xref::liveness_registry().iter().enumerate() {
        let status = match progress_island(index, model, live) {
            Ok((island, states)) => {
                std::fs::write(
                    format!("{out}/progress/{index:03}_{}.lean", live.name),
                    island,
                )
                .expect("write the progress island");
                format!("{states}\temitted")
            }
            Err(why) => format!("-\tnot emitted: {why}"),
        };
        let _ = writeln!(progress, "{index}\t{}\t{}\t{status}", model.name, live.name);
    }
    std::fs::write(format!("{out}/progress.tsv"), progress).expect("write the progress manifest");
}
