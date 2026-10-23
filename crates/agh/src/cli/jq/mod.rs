// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Built-in `jq` subset (`lex`, `parse`, `eval`) and `agh help jq` metadata tables.

mod eval;
mod lex;
mod parse;

use crate::error::{Error, Result};

/// One built-in function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Func {
    /// `select(f)`.
    Select,
    /// `map(f)`.
    Map,
    /// `length`.
    Length,
    /// `keys`.
    Keys,
    /// `has(k)`.
    Has,
    /// `tostring`.
    Tostring,
    /// `tonumber`.
    Tonumber,
    /// `type`.
    Type,
    /// `not`.
    Not,
    /// `empty`.
    Empty,
    /// `startswith(s)`.
    Startswith,
    /// `endswith(s)`.
    Endswith,
    /// `ltrimstr(s)`.
    Ltrimstr,
    /// `rtrimstr(s)`.
    Rtrimstr,
    /// `split(s)`.
    Split,
    /// `join(s)`.
    Join,
    /// `ascii_downcase`.
    AsciiDowncase,
    /// `ascii_upcase`.
    AsciiUpcase,
    /// `test(re)`.
    Test,
    /// `first` and `first(f)`.
    First,
    /// `last` and `last(f)`.
    Last,
    /// `sort`.
    Sort,
    /// `sort_by(f)`.
    SortBy,
    /// `unique`.
    Unique,
    /// `min`.
    Min,
    /// `max`.
    Max,
    /// `add`.
    Add,
    /// `any` and `any(f)`.
    Any,
    /// `all` and `all(f)`.
    All,
    /// `to_entries`.
    ToEntries,
    /// `from_entries`.
    FromEntries,
}

/// One row of `FUNCTIONS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FuncSpec {
    /// The name.
    pub(crate) name: &'static str,
    /// The argument counts it takes.
    pub(crate) arities: &'static [usize],
    /// What runs it.
    pub(crate) func: Func,
}

const fn f(name: &'static str, arities: &'static [usize], func: Func) -> FuncSpec {
    FuncSpec {
        name,
        arities,
        func,
    }
}

/// Every function of the subset, with its arities.
pub(crate) static FUNCTIONS: &[FuncSpec] = &[
    f("select", &[1], Func::Select),
    f("map", &[1], Func::Map),
    f("length", &[0], Func::Length),
    f("keys", &[0], Func::Keys),
    f("has", &[1], Func::Has),
    f("tostring", &[0], Func::Tostring),
    f("tonumber", &[0], Func::Tonumber),
    f("type", &[0], Func::Type),
    f("not", &[0], Func::Not),
    f("empty", &[0], Func::Empty),
    f("startswith", &[1], Func::Startswith),
    f("endswith", &[1], Func::Endswith),
    f("ltrimstr", &[1], Func::Ltrimstr),
    f("rtrimstr", &[1], Func::Rtrimstr),
    f("split", &[1], Func::Split),
    f("join", &[1], Func::Join),
    f("ascii_downcase", &[0], Func::AsciiDowncase),
    f("ascii_upcase", &[0], Func::AsciiUpcase),
    f("test", &[1], Func::Test),
    f("first", &[0, 1], Func::First),
    f("last", &[0, 1], Func::Last),
    f("sort", &[0], Func::Sort),
    f("sort_by", &[1], Func::SortBy),
    f("unique", &[0], Func::Unique),
    f("min", &[0], Func::Min),
    f("max", &[0], Func::Max),
    f("add", &[0], Func::Add),
    f("any", &[0, 1], Func::Any),
    f("all", &[0, 1], Func::All),
    f("to_entries", &[0], Func::ToEntries),
    f("from_entries", &[0], Func::FromEntries),
];

/// The formats (`@NAME`), each applied to `.`.
pub(crate) static FORMATS: &[&str] = &["tsv", "csv", "json", "text", "base64", "base64d"];

/// One row of `REFUSED_JQ`: the constructs, as a program spells them, and the rewrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RefusedJq {
    /// The refused names and tokens.
    pub(crate) names: &'static [&'static str],
    /// What to write instead.
    pub(crate) next: &'static str,
}

const fn r(names: &'static [&'static str], next: &'static str) -> RefusedJq {
    RefusedJq { names, next }
}

/// Every construct refused by name.
pub(crate) static REFUSED_JQ: &[RefusedJq] = &[
    r(&["def", "reduce", "foreach"], "use map, add or select"),
    r(&["..", "recurse"], "name the path"),
    r(
        &["=", "|=", "+=", "-=", "*=", "/=", "%=", "//="],
        "agh prints; it does not edit",
    ),
    r(&["try", "catch", "error"], "use ? or //"),
    r(&["label", "break"], "use first or select"),
    r(
        &[
            "path",
            "paths",
            "getpath",
            "setpath",
            "delpaths",
            "leaf_paths",
        ],
        "name the path",
    ),
    r(
        &["input", "inputs", "$ENV", "env"],
        "agh reads one document",
    ),
    r(
        &["match", "capture", "sub", "gsub", "scan", "splits"],
        "use test()",
    ),
    r(
        &[
            "now", "todate", "fromdate", "strftime", "strptime", "mktime", "gmtime", "date",
        ],
        "compare the RFC 3339 strings",
    ),
    r(&["@sh", "@uri", "@html"], "quote it in the shell"),
    r(&["limit", "until", "while", "range"], "use .[0:N]"),
    r(&["as [", "as {"], "bind one value with as $name"),
    r(&["?//"], "use //"),
];

/// Rules shared with `jq` 1.7.1, listed in `agh help jq`.
pub(crate) static SHARED_RULES: &[&str] = &[
    "string slices index by Unicode scalar",
    "null + x is x",
    ".[]? on a value that cannot be iterated yields nothing",
    "sort uses jq's type order: null, false, true, numbers, strings, arrays, objects",
];

/// Intentional differences from `jq` 1.7.1, listed in `agh help jq`.
pub(crate) static DIVERGENCES: &[&str] = &[
    "error texts are agh's own",
    "objects iterate, and tostring, @json, @text and \\(...) write them, in sorted key order",
    "a // b swallows the errors of a, where jq 1.7.1 raises them",
    "@base64d skips ASCII whitespace, so GitHub's 60-column lines decode",
    "a negative fractional index is floored, so .[-0.5] is the last item; jq truncates it",
    "from_entries also takes k, K, v and V, after jq's key, Key, name, Name, value and Value",
    "an unbound $x fails only where it is evaluated; jq refuses the program",
    "numbers print by value, not as written: 1.50 as 1.5, 1e3 as 1000, -0 as 0",
    "a computed double prints in its shortest form: 0.00001 + 0 is 0.00001, where jq prints 1e-05",
    "test() is aterm-regex: $ matches only at the very end; look-around, \\1 and \\Z do not compile",
    "test()'s POSIX classes such as [[:alpha:]] are ASCII-only, where jq's take Unicode letters",
    "test()'s (?i) folds one character at a time, so ß is not ss; \\u0078 in a pattern is x",
    "test() fails, rather than guess, when a search outruns aterm-regex's step budget",
    "a double tonumber reads prints as one: \"2.5e1\" | tonumber is 25.0, where jq prints 25",
    "tonumber reads JSON number text: \"nan\", \"+1\", \"01\", \"1.\" and \".5\" fail, where jq reads them",
    "\"1e1000\" | tonumber prints the largest double, 1.7976931348623157e+308; jq prints 1E+1000",
    "integer arithmetic is exact past 2^53, where jq 1.7.1 rounds through doubles",
    "a program nests at most 128 deep, counting each link of a chain such as a, b, c or .a.b.c",
];

/// Maximum nesting depth for `jq` expressions.
pub(crate) const MAX_DEPTH: usize = 128;

fn too_deep(at: usize) -> Error {
    syntax(&format!("the program nests deeper than {MAX_DEPTH}"), at)
}

const HELP_JQ: &str = "agh help jq";

fn syntax(what: &str, at: usize) -> Error {
    Error::usage(format!("jq: {what} at byte {at}")).with_next(HELP_JQ)
}

fn runtime(what: &str) -> Error {
    Error::usage(format!("jq: {what}")).with_next(HELP_JQ)
}

fn not_in_subset(name: &str, next: &str) -> Error {
    Error::usage(format!("jq: {name} is not in agh's jq subset")).with_next(next)
}

fn refused(construct: &str) -> Error {
    refused_name(construct).unwrap_or_else(|| not_in_subset(construct, HELP_JQ))
}

fn refused_name(name: &str) -> Option<Error> {
    REFUSED_JQ
        .iter()
        .find(|row| row.names.contains(&name))
        .map(|row| not_in_subset(name, row.next))
}

fn unknown_name(name: &str) -> Error {
    not_in_subset(name, HELP_JQ)
}

fn find_function(name: &str) -> Option<&'static FuncSpec> {
    FUNCTIONS.iter().find(|spec| spec.name == name)
}

/// A compiled program.
pub struct Program {
    ast: parse::Ast,
}

/// Compiles a `jq` filter expression.
///
/// # Errors
///
/// Returns a usage error on syntax errors or refused constructs.
pub fn compile(src: &str) -> Result<Program> {
    let tokens = lex::lex(src)?;
    let ast = parse::parse(&tokens, src.len())?;
    Ok(Program { ast })
}

impl Program {
    /// Evaluates the filter against `input` and returns the results in order.
    ///
    /// # Errors
    ///
    /// Returns a usage error on any runtime error (a bad type or index, an undefined `$name`, …).
    pub fn run(&self, input: &aterm_json::Value) -> Result<Vec<aterm_json::Value>> {
        let mut out = Vec::new();
        eval::run(&self.ast, input, &mut out)?;
        Ok(out)
    }
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Program")
    }
}

/// Formats results like `jq -r` (raw strings and compact sorted-key JSON, newline-terminated).
#[must_use]
pub fn render_results(results: &[aterm_json::Value]) -> String {
    let mut out = String::new();
    for value in results {
        match value {
            aterm_json::Value::String(text) => out.push_str(text),
            other => out.push_str(&json_text(other)),
        }
        out.push('\n');
    }
    out
}

/// Formats `value` as compact JSON with DEL escaped as `\u007f`.
pub(crate) fn json_text(value: &aterm_json::Value) -> String {
    let text = value.to_string();
    if text.contains('\u{7f}') {
        text.replace('\u{7f}', "\\u007f")
    } else {
        text
    }
}
