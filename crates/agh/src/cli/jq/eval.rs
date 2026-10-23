// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The jq evaluator: each expression is a stream of values, written into an output vector in order.

use std::cmp::Ordering;

use aterm_json::{Map, Number, Value};

use super::lex::CmpOp;
use super::parse::{ArithOp, Ast, StrPiece};
use super::{Func, json_text, runtime};
use crate::error::{Error, Result};

/// The variables in scope: a chain of `as $name` bindings, innermost first.
struct Env<'a> {
    name: &'a str,
    value: &'a Value,
    parent: Option<&'a Env<'a>>,
}

impl Env<'_> {
    fn get(&self, name: &str) -> Option<&Value> {
        let mut env = Some(self);
        while let Some(e) = env {
            if e.name == name {
                return Some(e.value);
            }
            env = e.parent;
        }
        None
    }
}

/// Run `ast` on `input`, appending its outputs to `out`.
pub(super) fn run(ast: &Ast, input: &Value, out: &mut Vec<Value>) -> Result<()> {
    eval(ast, input, None, out)
}

/// Every output of `ast`, or its error: for the constructs that fail whole (`[e]`, `map`,
/// `sort_by`, `last(f)`).
fn collect(ast: &Ast, input: &Value, env: Option<&Env<'_>>) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    eval(ast, input, env, &mut out)?;
    Ok(out)
}

/// The outputs `ast` wrote before it stopped, and how it stopped.
fn stream(ast: &Ast, input: &Value, env: Option<&Env<'_>>) -> (Vec<Value>, Result<()>) {
    let mut out = Vec::new();
    let status = eval(ast, input, env, &mut out);
    (out, status)
}

/// The inner side of a product, which jq runs again for each output of the outer side. Its
/// outputs are the same each time, so it runs once, on first use, and its error is raised the
/// first time its outputs are used up, where jq first meets it. A missing slice bound is
/// `null`.
struct Inner<'a> {
    ast: Option<&'a Ast>,
    run: Option<(Vec<Value>, Option<Error>)>,
}

impl<'a> Inner<'a> {
    const fn new(ast: Option<&'a Ast>) -> Self {
        Inner { ast, run: None }
    }

    /// The outputs, run on first use.
    fn values(&mut self, input: &Value, env: Option<&Env<'_>>) -> &[Value] {
        let ast = self.ast;
        let run = self.run.get_or_insert_with(|| match ast {
            Some(ast) => {
                let (values, status) = stream(ast, input, env);
                (values, status.err())
            }
            None => (vec![Value::Null], None),
        });
        &run.0
    }

    /// The error that ended the outputs, once they have been used up.
    fn end(&mut self) -> Result<()> {
        match self.run.as_mut().and_then(|run| run.1.take()) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// `lhs op rhs`, as jq runs a binary operator: the right side outermost and the left side
/// innermost, each result written as it is made. The left side's error stops the stream once
/// the first right output's results are out; the right side's once all of its are.
fn binop(
    lhs: &Ast,
    rhs: &Ast,
    input: &Value,
    env: Option<&Env<'_>>,
    out: &mut Vec<Value>,
    op: impl Fn(&Value, &Value) -> Result<Value>,
) -> Result<()> {
    let (rights, status) = stream(rhs, input, env);
    let mut lefts = Inner::new(Some(lhs));
    for r in &rights {
        for l in lefts.values(input, env) {
            out.push(op(l, r)?);
        }
        lefts.end()?;
    }
    status
}

/// `t[k]`, or nothing when the step is optional and `t` cannot be indexed with `k`.
fn step(result: Result<Value>, opt: bool, out: &mut Vec<Value>) -> Result<()> {
    match result {
        Ok(value) => out.push(value),
        Err(_) if opt => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// jq's truthiness: everything but `false` and `null`.
const fn truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

/// jq's type name.
const fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `{type} ({value})`, as jq's errors name a value: the value cut to 30 characters.
fn shown(value: &Value) -> String {
    let text = value.to_string();
    let cut: String = text.chars().take(30).collect();
    if cut.len() < text.len() {
        format!("{} ({cut}...)", type_name(value))
    } else {
        format!("{} ({cut})", type_name(value))
    }
}

/// Compact JSON for anything but a string, which is itself.
fn tostring(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => json_text(other),
    }
}

fn eval(ast: &Ast, input: &Value, env: Option<&Env<'_>>, out: &mut Vec<Value>) -> Result<()> {
    match ast {
        Ast::Identity => out.push(input.clone()),
        Ast::Literal(value) => out.push(value.clone()),
        Ast::Str(pieces) => interpolate(pieces, input, env, out)?,
        Ast::Var(name) => match env.and_then(|e| e.get(name)) {
            Some(value) => out.push(value.clone()),
            None => return Err(runtime(&format!("${name} is not defined"))),
        },
        Ast::Index(target, key, opt) => {
            let (keys, status) = stream(key, input, env);
            let mut targets = Inner::new(Some(target));
            for k in &keys {
                for t in targets.values(input, env) {
                    step(index(t, k), *opt, out)?;
                }
                targets.end()?;
            }
            status?;
        }
        Ast::Slice(target, lo, hi, opt) => {
            let mut los = Inner::new(lo.as_deref());
            let mut his = Inner::new(hi.as_deref());
            let mut targets = Inner::new(Some(target));
            for lo in los.values(input, env) {
                for hi in his.values(input, env) {
                    for t in targets.values(input, env) {
                        step(slice(t, lo, hi), *opt, out)?;
                    }
                    targets.end()?;
                }
                his.end()?;
            }
            los.end()?;
        }
        Ast::Iterate(target, opt) => {
            let (targets, status) = stream(target, input, env);
            for t in &targets {
                match items_of(t, None) {
                    Ok(items) => out.extend(items),
                    Err(_) if *opt => {}
                    Err(e) => return Err(e),
                }
            }
            status?;
        }
        Ast::Try(inner) => {
            let _ = eval(inner, input, env, out);
        }
        Ast::Pipe(lhs, rhs) => {
            let mut left = Vec::new();
            let status = eval(lhs, input, env, &mut left);
            for value in &left {
                eval(rhs, value, env, out)?;
            }
            status?;
        }
        Ast::Comma(lhs, rhs) => {
            eval(lhs, input, env, out)?;
            eval(rhs, input, env, out)?;
        }
        Ast::Alt(lhs, rhs) => {
            let mut left = Vec::new();
            let _ = eval(lhs, input, env, &mut left);
            let kept: Vec<Value> = left.into_iter().filter(truthy).collect();
            if kept.is_empty() {
                eval(rhs, input, env, out)?;
            } else {
                out.extend(kept);
            }
        }
        Ast::Or(lhs, rhs) | Ast::And(lhs, rhs) => {
            let decides = matches!(ast, Ast::Or(..));
            let (lefts, status) = stream(lhs, input, env);
            for l in &lefts {
                if truthy(l) == decides {
                    out.push(Value::Bool(decides));
                } else {
                    let (rights, rstatus) = stream(rhs, input, env);
                    out.extend(rights.iter().map(|r| Value::Bool(truthy(r))));
                    rstatus?;
                }
            }
            status?;
        }
        Ast::Cmp(op, lhs, rhs) => binop(lhs, rhs, input, env, out, |l, r| {
            let order = compare(l, r);
            Ok(Value::Bool(match op {
                CmpOp::Eq => order == Ordering::Equal,
                CmpOp::Ne => order != Ordering::Equal,
                CmpOp::Lt => order == Ordering::Less,
                CmpOp::Le => order != Ordering::Greater,
                CmpOp::Gt => order == Ordering::Greater,
                CmpOp::Ge => order != Ordering::Less,
            }))
        })?,
        Ast::Arith(op, lhs, rhs) => binop(lhs, rhs, input, env, out, |l, r| match op {
            ArithOp::Add => add(l, r),
            ArithOp::Sub => sub(l, r),
        })?,
        Ast::Array(inner) => {
            let items = match inner {
                Some(inner) => collect(inner, input, env)?,
                None => Vec::new(),
            };
            out.push(Value::Array(items));
        }
        Ast::Object(entries) => object(entries, input, env, out)?,
        Ast::Call(func, args) => call(*func, args, input, env, out)?,
        Ast::Format(name) => out.push(Value::String(format(name, input)?)),
        Ast::Bind(source, name, body) => {
            let mut values = Vec::new();
            let status = eval(source, input, env, &mut values);
            for value in &values {
                let inner = Env {
                    name,
                    value,
                    parent: env,
                };
                eval(body, input, Some(&inner), out)?;
            }
            status?;
        }
    }
    Ok(())
}

/// A string's every rendering: the cartesian product of its interpolations' outputs, written
/// before the error that stopped it.
fn interpolate(
    pieces: &[StrPiece],
    input: &Value,
    env: Option<&Env<'_>>,
    out: &mut Vec<Value>,
) -> Result<()> {
    let mut texts = vec![String::new()];
    let mut error: Option<Error> = None;
    for piece in pieces {
        match piece {
            StrPiece::Lit(lit) => {
                for text in &mut texts {
                    text.push_str(lit);
                }
            }
            StrPiece::Interp(ast) => {
                let (values, status) = stream(ast, input, env);
                let mut next = Vec::with_capacity(texts.len() * values.len());
                let mut next_error = None;
                for value in &values {
                    let value = tostring(value);
                    for text in &texts {
                        next.push(format!("{text}{value}"));
                    }
                    if let Some(e) = error.take() {
                        next_error = Some(e);
                        break;
                    }
                }
                texts = next;
                error = next_error.or_else(|| status.err());
            }
        }
    }
    out.extend(texts.into_iter().map(Value::String));
    error.map_or(Ok(()), Err)
}

/// One object entry's `(key, value)` pairs, in jq's order (the key outermost), and the error
/// that ended them. A key that is not a string fails where jq's insert meets it: at its first
/// value.
fn object_entry(
    key: &Ast,
    value: &Ast,
    input: &Value,
    env: Option<&Env<'_>>,
) -> (Vec<(String, Value)>, Option<Error>) {
    let (keys, status) = stream(key, input, env);
    let mut values = Inner::new(Some(value));
    let mut pairs = Vec::new();
    for k in &keys {
        for v in values.values(input, env) {
            let Value::String(k) = k else {
                let e = runtime(&format!("object keys must be strings, not {}", shown(k)));
                return (pairs, Some(e));
            };
            pairs.push((k.clone(), v.clone()));
        }
        if let Err(e) = values.end() {
            return (pairs, Some(e));
        }
    }
    (pairs, status.err())
}

/// `{k: v, …}`: the cartesian product of its entries' pairs, the first entry outermost, each
/// object written as it is made. An entry runs only when the entries before it have a pair,
/// and its error stops the stream the first time its pairs are used up. An odometer over the
/// entries, with no recursion, so the number of entries is unbounded.
fn object(
    entries: &[(Ast, Ast)],
    input: &Value,
    env: Option<&Env<'_>>,
    out: &mut Vec<Value>,
) -> Result<()> {
    let Some((key, value)) = entries.first() else {
        out.push(Value::Object(Map::new()));
        return Ok(());
    };
    let mut ran = vec![object_entry(key, value, input, env)];
    let mut at = vec![0usize];
    while let Some(&i) = at.last() {
        let depth = at.len() - 1;
        if i < ran[depth].0.len() {
            if depth + 1 == entries.len() {
                let mut map = Map::new();
                for (d, &p) in at.iter().enumerate() {
                    let (k, v) = &ran[d].0[p];
                    map.insert(k.clone(), v.clone());
                }
                out.push(Value::Object(map));
                if let Some(last) = at.last_mut() {
                    *last += 1;
                }
            } else {
                if ran.len() == depth + 1 {
                    let (key, value) = &entries[depth + 1];
                    ran.push(object_entry(key, value, input, env));
                }
                at.push(0);
            }
        } else {
            if let Some(e) = ran[depth].1.take() {
                return Err(e);
            }
            at.pop();
            if let Some(last) = at.last_mut() {
                *last += 1;
            }
        }
    }
    Ok(())
}

/// `t[k]`.
fn index(target: &Value, key: &Value) -> Result<Value> {
    match (target, key) {
        (Value::Null, Value::String(_) | Value::Number(_) | Value::Null) => Ok(Value::Null),
        (Value::Object(map), Value::String(k)) => Ok(map.get(k).cloned().unwrap_or(Value::Null)),
        (Value::Array(items), Value::Number(n)) => {
            let i = n.as_f64().unwrap_or(0.0).floor();
            let len = items.len() as f64;
            let i = if i < 0.0 { i + len } else { i };
            if i < 0.0 || i >= len {
                return Ok(Value::Null);
            }
            Ok(items.get(i as usize).cloned().unwrap_or(Value::Null))
        }
        (_, Value::String(k)) => Err(runtime(&format!(
            "cannot index {} with \"{k}\"",
            type_name(target)
        ))),
        _ => Err(runtime(&format!(
            "cannot index {} with {}",
            type_name(target),
            type_name(key)
        ))),
    }
}

/// A slice bound, as a whole index into a sequence of `len`, as jq 1.7.1 takes it: a negative
/// counted from the end first, then a fraction floored for the start and raised for the end,
/// then clamped. So `[1,2,3][:-0.5]` ends at `ceil(2.5)`, and is the whole array.
fn slice_index(bound: &Value, len: usize, default: usize, end: bool) -> Result<usize> {
    let n = match bound {
        Value::Null => return Ok(default),
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        other => {
            return Err(runtime(&format!(
                "start and end indices of a slice must be numbers, not {}",
                type_name(other)
            )));
        }
    };
    let len_f = len as f64;
    let n = if n < 0.0 { n + len_f } else { n };
    let n = if end { n.ceil() } else { n.floor() };
    Ok(n.clamp(0.0, len_f) as usize)
}

/// `t[lo:hi]`, by Unicode scalar on a string.
fn slice(target: &Value, lo: &Value, hi: &Value) -> Result<Value> {
    match target {
        Value::Null => Ok(Value::Null),
        Value::Array(items) => {
            let start = slice_index(lo, items.len(), 0, false)?;
            let end = slice_index(hi, items.len(), items.len(), true)?;
            Ok(Value::Array(
                items.get(start..end.max(start)).unwrap_or(&[]).to_vec(),
            ))
        }
        Value::String(text) => {
            let chars: Vec<char> = text.chars().collect();
            let start = slice_index(lo, chars.len(), 0, false)?;
            let end = slice_index(hi, chars.len(), chars.len(), true)?;
            Ok(Value::String(
                chars
                    .get(start..end.max(start))
                    .unwrap_or(&[])
                    .iter()
                    .collect(),
            ))
        }
        other => Err(runtime(&format!(
            "cannot index {} with object",
            type_name(other)
        ))),
    }
}

/// A number's exact integer, when it has one.
fn integer(n: &Number) -> Option<i128> {
    n.as_u64()
        .map(i128::from)
        .or_else(|| n.as_i64().map(i128::from))
}

/// An integer as a JSON number, or a double when it fits neither `u64` nor `i64`.
fn from_integer(v: i128) -> Value {
    if let Ok(u) = u64::try_from(v) {
        Value::from(u)
    } else if let Ok(i) = i64::try_from(v) {
        Value::from(i)
    } else {
        Value::from(v as f64)
    }
}

/// Converts a double into a JSON number or null.
pub(super) fn from_double(v: f64) -> Value {
    const EXACT: f64 = 9_007_199_254_740_992.0;
    if v.is_finite() && v.fract() == 0.0 && v.abs() <= EXACT {
        from_integer(v as i128)
    } else if v.is_infinite() {
        Value::from(f64::MAX.copysign(v))
    } else {
        Value::from(v)
    }
}

/// Two numbers combined: exactly when both are integers, else as doubles.
fn arith(a: &Number, b: &Number, int: fn(i128, i128) -> i128, dbl: fn(f64, f64) -> f64) -> Value {
    match (integer(a), integer(b)) {
        (Some(x), Some(y)) => from_integer(int(x, y)),
        _ => from_double(dbl(a.as_f64().unwrap_or(0.0), b.as_f64().unwrap_or(0.0))),
    }
}

/// `a + b`.
fn add(a: &Value, b: &Value) -> Result<Value> {
    match (a, b) {
        (Value::Null, x) | (x, Value::Null) => Ok(x.clone()),
        (Value::Number(x), Value::Number(y)) => Ok(arith(x, y, |p, q| p + q, |p, q| p + q)),
        (Value::String(x), Value::String(y)) => Ok(Value::String(format!("{x}{y}"))),
        (Value::Array(x), Value::Array(y)) => {
            let mut items = x.clone();
            items.extend(y.iter().cloned());
            Ok(Value::Array(items))
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut map = x.clone();
            for (k, v) in y {
                map.insert(k.clone(), v.clone());
            }
            Ok(Value::Object(map))
        }
        _ => Err(runtime(&format!(
            "cannot add {} and {}",
            type_name(a),
            type_name(b)
        ))),
    }
}

/// `a - b`.
fn sub(a: &Value, b: &Value) -> Result<Value> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(arith(x, y, |p, q| p - q, |p, q| p - q)),
        (Value::Array(x), Value::Array(y)) => Ok(Value::Array(
            x.iter()
                .filter(|item| {
                    !y.iter()
                        .any(|other| compare(item, other) == Ordering::Equal)
                })
                .cloned()
                .collect(),
        )),
        _ => Err(runtime(&format!(
            "cannot subtract {} and {}",
            type_name(a),
            type_name(b)
        ))),
    }
}

/// A value's place in jq's type order.
const fn rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Number(_) => 3,
        Value::String(_) => 4,
        Value::Array(_) => 5,
        Value::Object(_) => 6,
    }
}

/// jq's total order: null < false < true < numbers < strings < arrays < objects; arrays
/// element-wise; objects by their sorted keys, then their values.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (integer(x), integer(y)) {
            (Some(p), Some(q)) => p.cmp(&q),
            _ => x
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&y.as_f64().unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
        },
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Array(x), Value::Array(y)) => {
            for (p, q) in x.iter().zip(y) {
                let order = compare(p, q);
                if order != Ordering::Equal {
                    return order;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            let order = x.keys().cmp(y.keys());
            if order != Ordering::Equal {
                return order;
            }
            for (p, q) in x.values().zip(y.values()) {
                let order = compare(p, q);
                if order != Ordering::Equal {
                    return order;
                }
            }
            Ordering::Equal
        }
        _ => rank(a).cmp(&rank(b)),
    }
}

/// The items `.[]` yields: an array's items, or an object's values in sorted key order. The
/// error names `what`, the function iterating its input, when there is one (`t[]` has none).
fn items_of(value: &Value, what: Option<&str>) -> Result<Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.values().cloned().collect()),
        other => {
            let who = what.map_or(String::new(), |what| format!("{what} "));
            Err(runtime(&format!(
                "{who}cannot iterate over {}",
                shown(other)
            )))
        }
    }
}

/// An array input, for the functions that sort.
fn array_of<'v>(value: &'v Value, what: &str) -> Result<&'v Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items),
        other => Err(runtime(&format!(
            "{what} wants an array, not {}",
            type_name(other)
        ))),
    }
}

/// A string input or argument.
fn string_of<'v>(value: &'v Value, what: &str) -> Result<&'v str> {
    match value {
        Value::String(text) => Ok(text),
        other => Err(runtime(&format!(
            "{what} wants a string, not {}",
            type_name(other)
        ))),
    }
}

/// `split(sep)` of `text`, as jq 1.7.1 splits: an empty input gives `[]`, an empty separator
/// every character.
fn split(text: &str, sep: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    if sep.is_empty() {
        return text.chars().map(|c| Value::String(c.to_string())).collect();
    }
    text.split(sep)
        .map(|part| Value::String(part.to_string()))
        .collect()
}

/// The first value of `obj` under any of `names` that is neither null nor false.
fn first_truthy<'v>(obj: &'v Map, names: &[&str]) -> Option<&'v Value> {
    names
        .iter()
        .filter_map(|name| obj.get(*name))
        .find(|v| truthy(v))
}

/// `from_entries`.
fn from_entries(input: &Value) -> Result<Value> {
    let mut map = Map::new();
    for entry in items_of(input, Some("from_entries"))? {
        let Value::Object(obj) = &entry else {
            return Err(runtime(&format!(
                "from_entries wants objects, not {}",
                type_name(&entry)
            )));
        };
        let key = match first_truthy(obj, &["key", "Key", "name", "Name", "k", "K"]) {
            Some(Value::String(key)) => key.clone(),
            Some(other) => {
                return Err(runtime(&format!(
                    "cannot use {} as an object key",
                    shown(other)
                )));
            }
            None => return Err(runtime("cannot use null (null) as an object key")),
        };
        let value = ["value", "Value", "v", "V"]
            .iter()
            .find_map(|name| obj.get(*name))
            .cloned()
            .unwrap_or(Value::Null);
        map.insert(key, value);
    }
    Ok(Value::Object(map))
}

/// Whether `f` holds for any (`want == true`) or all (`want == false`) of `items`. As jq
/// 1.7.1's `isempty(first(...))`, the first output that decides stops the search, so an error
/// `f` meets after it is never raised.
fn any_all(items: &[Value], f: Option<&Ast>, env: Option<&Env<'_>>, want: bool) -> Result<bool> {
    for item in items {
        let (outputs, status) = match f {
            Some(f) => stream(f, item, env),
            None => (vec![item.clone()], Ok(())),
        };
        if outputs.iter().any(|o| truthy(o) == want) {
            return Ok(want);
        }
        status?;
    }
    Ok(!want)
}

/// One function call.
fn call(
    func: Func,
    args: &[Ast],
    input: &Value,
    env: Option<&Env<'_>>,
    out: &mut Vec<Value>,
) -> Result<()> {
    let arg = |i: usize| -> (Vec<Value>, Result<()>) {
        match args.get(i) {
            Some(ast) => stream(ast, input, env),
            None => (Vec::new(), Ok(())),
        }
    };
    match func {
        Func::Select => {
            let (outputs, status) = arg(0);
            for o in &outputs {
                if truthy(o) {
                    out.push(input.clone());
                }
            }
            status?;
        }
        Func::Map => {
            let mut mapped = Vec::new();
            if let Some(f) = args.first() {
                for item in items_of(input, Some("map"))? {
                    eval(f, &item, env, &mut mapped)?;
                }
            }
            out.push(Value::Array(mapped));
        }
        Func::Length => out.push(match input {
            Value::Null => Value::from(0u64),
            Value::Bool(_) => {
                return Err(runtime(&format!("{} has no length", shown(input))));
            }
            Value::Number(n) => match integer(n) {
                Some(i) => from_integer(i.abs()),
                None => from_double(n.as_f64().unwrap_or(0.0).abs()),
            },
            Value::String(text) => Value::from(text.chars().count() as u64),
            Value::Array(items) => Value::from(items.len() as u64),
            Value::Object(map) => Value::from(map.len() as u64),
        }),
        Func::Keys => out.push(match input {
            Value::Object(map) => Value::Array(map.keys().cloned().map(Value::String).collect()),
            Value::Array(items) => Value::Array((0..items.len() as u64).map(Value::from).collect()),
            other => return Err(runtime(&format!("{} has no keys", shown(other)))),
        }),
        Func::Has => {
            let (keys, status) = arg(0);
            for k in &keys {
                out.push(Value::Bool(match (input, k) {
                    (Value::Object(map), Value::String(k)) => map.contains_key(k),
                    (Value::Array(items), Value::Number(n)) => {
                        let i = n.as_f64().map_or(-1.0, f64::trunc);
                        i >= 0.0 && i < items.len() as f64
                    }
                    (Value::Null, _) => false,
                    _ => {
                        return Err(runtime(&format!(
                            "cannot check whether {} has a {} key",
                            type_name(input),
                            type_name(k)
                        )));
                    }
                }));
            }
            status?;
        }
        Func::Tostring => out.push(Value::String(tostring(input))),
        Func::Tonumber => out.push(match input {
            Value::Number(_) => input.clone(),
            Value::String(text) => match aterm_json::from_str::<Value>(text) {
                Ok(n @ Value::Number(_)) => n,
                _ => match text.trim_ascii().parse::<f64>() {
                    Ok(v) if v.is_infinite() => from_double(v),
                    _ => {
                        return Err(runtime(&format!(
                            "{} cannot be parsed as a number",
                            shown(input)
                        )));
                    }
                },
            },
            other => {
                return Err(runtime(&format!(
                    "{} cannot be parsed as a number",
                    shown(other)
                )));
            }
        }),
        Func::Type => out.push(Value::String(type_name(input).to_string())),
        Func::Not => out.push(Value::Bool(!truthy(input))),
        Func::Empty => {}
        Func::Startswith | Func::Endswith => {
            let name = if func == Func::Startswith {
                "startswith"
            } else {
                "endswith"
            };
            let (prefixes, status) = arg(0);
            for s in &prefixes {
                let text = string_of(input, name)?;
                let s = string_of(s, name)?;
                out.push(Value::Bool(if func == Func::Startswith {
                    text.starts_with(s)
                } else {
                    text.ends_with(s)
                }));
            }
            status?;
        }
        Func::Ltrimstr | Func::Rtrimstr => {
            let (affixes, status) = arg(0);
            for s in &affixes {
                let trimmed = match (input, s) {
                    (Value::String(text), Value::String(s)) => if func == Func::Ltrimstr {
                        text.strip_prefix(s.as_str())
                    } else {
                        text.strip_suffix(s.as_str())
                    }
                    .map(|t| Value::String(t.to_string())),
                    _ => None,
                };
                out.push(trimmed.unwrap_or_else(|| input.clone()));
            }
            status?;
        }
        Func::Split => {
            let (seps, status) = arg(0);
            for sep in &seps {
                let text = string_of(input, "split")?;
                out.push(Value::Array(split(text, string_of(sep, "split")?)));
            }
            status?;
        }
        Func::Join => {
            let (seps, status) = arg(0);
            for sep in &seps {
                let items = items_of(input, Some("join"))?;
                let sep = string_of(sep, "join")?;
                let mut joined = String::new();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        joined.push_str(sep);
                    }
                    match item {
                        Value::Null => {}
                        Value::String(text) => joined.push_str(text),
                        Value::Number(_) | Value::Bool(_) => joined.push_str(&item.to_string()),
                        other => {
                            return Err(runtime(&format!("cannot join with {}", type_name(other))));
                        }
                    }
                }
                out.push(Value::String(joined));
            }
            status?;
        }
        Func::AsciiDowncase => out.push(Value::String(
            string_of(input, "ascii_downcase")?.to_ascii_lowercase(),
        )),
        Func::AsciiUpcase => out.push(Value::String(
            string_of(input, "ascii_upcase")?.to_ascii_uppercase(),
        )),
        Func::Test => {
            let (patterns, status) = arg(0);
            for re in &patterns {
                let text = string_of(input, "test")?;
                let pattern = string_of(re, "test")?;
                let regex = aterm_regex::Regex::new(pattern)
                    .map_err(|e| runtime(&format!("test() cannot compile {pattern:?}: {e}")))?;
                let matched = regex.try_is_match(text).map_err(|e| {
                    runtime(&format!(
                        "test({pattern:?}) gave up on a {}-byte string: {e}",
                        text.len()
                    ))
                })?;
                out.push(Value::Bool(matched));
            }
            status?;
        }
        Func::First => match args.first() {
            None => out.push(index(input, &Value::from(0u64))?),
            Some(f) => {
                let mut outputs = Vec::new();
                let status = eval(f, input, env, &mut outputs);
                match outputs.into_iter().next() {
                    Some(first) => out.push(first),
                    None => status?,
                }
            }
        },
        Func::Last => match args.first() {
            None => out.push(index(input, &Value::from(-1i64))?),
            Some(f) => out.push(collect(f, input, env)?.pop().unwrap_or(Value::Null)),
        },
        Func::Sort => {
            let mut items = array_of(input, "sort")?.clone();
            items.sort_by(compare);
            out.push(Value::Array(items));
        }
        Func::SortBy => {
            let items = array_of(input, "sort_by")?;
            let mut keyed = Vec::with_capacity(items.len());
            for item in items {
                let key = match args.first() {
                    Some(f) => Value::Array(collect(f, item, env)?),
                    None => Value::Null,
                };
                keyed.push((key, item.clone()));
            }
            keyed.sort_by(|a, b| compare(&a.0, &b.0));
            out.push(Value::Array(keyed.into_iter().map(|(_, v)| v).collect()));
        }
        Func::Unique => {
            let mut items = array_of(input, "unique")?.clone();
            items.sort_by(compare);
            items.dedup_by(|a, b| compare(a, b) == Ordering::Equal);
            out.push(Value::Array(items));
        }
        Func::Min | Func::Max => {
            let items = array_of(input, if func == Func::Min { "min" } else { "max" })?;
            let pick = items.iter().reduce(|best, item| {
                let order = compare(item, best);
                let better = if func == Func::Min {
                    order == Ordering::Less
                } else {
                    order != Ordering::Less
                };
                if better { item } else { best }
            });
            out.push(pick.cloned().unwrap_or(Value::Null));
        }
        Func::Add => {
            let mut sum = Value::Null;
            for item in items_of(input, Some("add"))? {
                sum = add(&sum, &item)?;
            }
            out.push(sum);
        }
        Func::Any | Func::All => {
            let name = if func == Func::Any { "any" } else { "all" };
            let items = items_of(input, Some(name))?;
            out.push(Value::Bool(any_all(
                &items,
                args.first(),
                env,
                func == Func::Any,
            )?));
        }
        Func::ToEntries => match input {
            Value::Object(map) => out.push(Value::Array(
                map.iter()
                    .map(|(k, v)| {
                        let mut entry = Map::new();
                        entry.insert("key".to_string(), Value::String(k.clone()));
                        entry.insert("value".to_string(), v.clone());
                        Value::Object(entry)
                    })
                    .collect(),
            )),
            Value::Array(items) => out.push(Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let mut entry = Map::new();
                        entry.insert("key".to_string(), Value::from(i as u64));
                        entry.insert("value".to_string(), v.clone());
                        Value::Object(entry)
                    })
                    .collect(),
            )),
            other => {
                return Err(runtime(&format!(
                    "to_entries wants an object or an array, not {}",
                    type_name(other)
                )));
            }
        },
        Func::FromEntries => out.push(from_entries(input)?),
    }
    Ok(())
}

/// One `@tsv` cell: jq's escapes for a string, JSON for a number, `true`/`false`, and an empty
/// cell for null.
fn tsv_cell(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => String::new(),
        Value::String(text) => {
            let mut cell = String::with_capacity(text.len());
            for c in text.chars() {
                match c {
                    '\\' => cell.push_str("\\\\"),
                    '\t' => cell.push_str("\\t"),
                    '\n' => cell.push_str("\\n"),
                    '\r' => cell.push_str("\\r"),
                    '\0' => cell.push_str("\\0"),
                    c => cell.push(c),
                }
            }
            cell
        }
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        other => {
            return Err(runtime(&format!(
                "{} is not valid in a tsv row",
                shown(other)
            )));
        }
    })
}

/// One `@csv` cell: a string quoted with its quotes doubled and a NUL written `\0`, as jq
/// 1.7.1 does; a number or bool bare; null empty.
fn csv_cell(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => String::new(),
        Value::String(text) => format!("\"{}\"", text.replace('"', "\"\"").replace('\0', "\\0")),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        other => {
            return Err(runtime(&format!(
                "{} is not valid in a csv row",
                shown(other)
            )));
        }
    })
}

/// `@name` applied to `input`.
fn format(name: &str, input: &Value) -> Result<String> {
    match name {
        "tsv" | "csv" => {
            let Value::Array(items) = input else {
                return Err(runtime(&format!(
                    "{} cannot be {name}-formatted, only an array can be",
                    shown(input)
                )));
            };
            let cells = items
                .iter()
                .map(if name == "tsv" { tsv_cell } else { csv_cell })
                .collect::<Result<Vec<String>>>()?;
            Ok(cells.join(if name == "tsv" { "\t" } else { "," }))
        }
        "json" => Ok(json_text(input)),
        "base64" => aterm_codec::base64::encode(tostring(input).as_bytes())
            .map_err(|e| runtime(&format!("@base64: {e}"))),
        "base64d" => {
            let text: String = tostring(input)
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            let data = text.split('=').next().unwrap_or_default();
            let bytes = aterm_codec::base64::decode(data)
                .map_err(|_| runtime(&format!("{} is not valid base64 data", shown(input))))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => Ok(tostring(input)),
    }
}
