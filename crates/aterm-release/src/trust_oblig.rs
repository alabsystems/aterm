// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-obligation Trust verification records ([`Record`], [`REPORT_SCHEMA`], [`write_record`],
//! [`reduce`], [`Obligation::file`], [`anchor_of`]).
//!
//! Each row keeps what stopped its obligation ([`Obligation::stop`]): the judge credits a
//! shortfall only with obligations a backstop stopped in this run and not in the run that set
//! the floor ([`Obligation::key`], `crate::trust_facts`), so a lapse every run has is the code's.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;
use serde::Deserialize;

use crate::trust_facts::{hex, one_line, one_line_owned};

/// The record's first line.
pub const RECORD_MAGIC: &str = "# aterm trust obligations 2";
/// The one report schema [`reduce`] reads.
pub const REPORT_SCHEMA: &str = "trust.report.ndjson.v2";
/// The file of an obligation the report gives no location.
pub const NO_LOCATION: &str = "@none";
/// What the record of `<spec>.log` is called, beside it.
pub const RECORD_SUFFIX: &str = ".oblig.tsv";
/// The stop of an obligation nothing stopped early, or whose report named no stop.
pub const NO_STOP: &str = "-";

/// Whether `stop` is one of the prover's backstops: its wall clocks' (`wall-backstop`) or its
/// RSS reading's (`rss-backstop`), which a busier machine hits more often.
#[must_use]
pub fn is_backstop(stop: &str) -> bool {
    stop.ends_with("-backstop")
}

/// The standard library's crates, as a toolchain prints their sources.
const SYSROOT: &[&str] = &[
    "alloc",
    "core",
    "std",
    "proc_macro",
    "test",
    "panic_abort",
    "panic_unwind",
    "unwind",
    "stdarch",
    "portable-simd",
    "backtrace",
    "profiler_builtins",
    "compiler-builtins",
    "rtstartup",
    "sysroot",
    "rustc-std-workspace-core",
    "rustc-std-workspace-alloc",
    "rustc-std-workspace-std",
];

/// Where an obligation is, as the report gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub l0: u32,
    pub c0: u32,
    pub l1: u32,
    pub c1: u32,
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}-{}:{}", self.l0, self.c0, self.l1, self.c1)
    }
}

/// One obligation of one check, normalised ([`reduce`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obligation {
    /// The function's path, its `cargo-target(…)::` unit prefix removed.
    pub function: String,
    pub kind: String,
    /// Repo-relative, `@sysroot/…`, `@registry/…`, or [`NO_LOCATION`].
    pub file: String,
    pub span: Span,
    /// The report's `outcome.status`; only `proved` is proved.
    pub status: String,
    /// What stopped it, as the report's `[stop:…]` tag names it (`wall-backstop` for a
    /// `timeout` that names none), or [`NO_STOP`].
    pub stop: String,
    /// The span's source text ([`anchor_of`]); `-` out of the tree.
    pub anchor: String,
    /// The report's description, whitespace collapsed.
    pub desc: String,
}

impl Obligation {
    #[must_use]
    pub fn proved(&self) -> bool {
        self.status == "proved"
    }

    /// Left unproved because a backstop stopped it ([`is_backstop`]).
    #[must_use]
    pub fn backstopped(&self) -> bool {
        !self.proved() && is_backstop(&self.stop)
    }

    /// Its name across runs: its function, kind, file, anchor and description (a MIR local and
    /// an interned hash erased), never its line, which an edit above it moves. Look-alikes
    /// share a key, so runs are compared as multisets of keys.
    #[must_use]
    pub fn key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.function.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.kind.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.file.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.anchor.as_bytes());
        hasher.update(b"\0");
        visit_norm_desc(&self.desc, |chunk| hasher.update(chunk.as_bytes()));
        hex(&hasher.finalize()[..8])
    }

    /// One line a reader finds it by: its function, kind, file and line, and its anchor in
    /// backquotes.
    #[must_use]
    pub fn said(&self) -> String {
        one_line_owned(format!(
            "{} {} {}:{} `{}`",
            self.function, self.kind, self.file, self.span.l0, self.anchor
        ))
    }
}

/// One check's obligations, as the lane recorded them beside its log, in tab-separated lines:
/// a header (`package`, `unit`, `functions`, `total`, `proved`), one
/// `o <function> <kind> <file> <l0> <c0> <l1> <c1> <status> <stop> <anchor> <description>`
/// row per obligation, and `end <total>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub package: String,
    pub unit: String,
    pub functions: u64,
    pub rows: Vec<Obligation>,
}

impl Record {
    #[must_use]
    pub fn proved(&self) -> u64 {
        self.rows.iter().filter(|o| o.proved()).count() as u64
    }

    #[must_use]
    pub const fn total(&self) -> u64 {
        self.rows.len() as u64
    }

    #[must_use]
    pub fn to_text(&self) -> String {
        let total = self.total();
        let proved = self.proved();
        let mut out = String::with_capacity(128 + self.rows.len() * 96);
        let _ = write!(
            out,
            "{RECORD_MAGIC}\npackage\t{}\nunit\t{}\nfunctions\t{}\ntotal\t{total}\nproved\t{proved}\n",
            self.package, self.unit, self.functions,
        );
        for o in &self.rows {
            let _ = writeln!(
                out,
                "o\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                o.function,
                o.kind,
                o.file,
                o.span.l0,
                o.span.c0,
                o.span.l1,
                o.span.c1,
                o.status,
                o.stop,
                o.anchor,
                o.desc
            );
        }
        let _ = writeln!(out, "end\t{total}");
        out
    }

    /// Read a record ([`Record::to_text`]), held to its shape: every field in its form, the
    /// header's counts the rows', and an `end` line (a record cut short has none).
    ///
    /// # Errors
    /// Why the record cannot be used, in one line.
    pub fn parse(text: &str) -> std::result::Result<Record, String> {
        let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));
        match lines.next() {
            Some((_, l)) if l == RECORD_MAGIC => {}
            _ => return Err(format!("its first line is not `{RECORD_MAGIC}`")),
        }
        let mut head = |want: &str| -> std::result::Result<&str, String> {
            match lines.next() {
                Some((_, l)) => match l.split_once('\t') {
                    Some((k, v)) if k == want && !v.is_empty() && !v.contains('\t') => Ok(v),
                    _ => Err(format!("its `{want}` line is missing or malformed")),
                },
                None => Err(format!("it ends before its `{want}` line")),
            }
        };
        let package = head("package")?.to_string();
        let unit = head("unit")?.to_string();
        let number = |s: &str, what: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
                .ok_or_else(|| format!("its `{what}` line is not a count"))
        };
        let functions = number(head("functions")?, "functions")?;
        let total = number(head("total")?, "total")?;
        let proved = number(head("proved")?, "proved")?;
        let mut rows = Vec::with_capacity(usize::try_from(total).unwrap_or(0).min(1_000_000));
        let mut end = None;
        for (n, line) in lines {
            if end.is_some() {
                return Err(format!("line {n} follows its `end` line"));
            }
            let mut f = line.split('\t');
            match f.next() {
                Some("o") => {
                    let (
                        Some(f0),
                        Some(f1),
                        Some(f2),
                        Some(f3),
                        Some(f4),
                        Some(f5),
                        Some(f6),
                        Some(f7),
                        Some(f8),
                        Some(f9),
                        Some(f10),
                        None,
                    ) = (
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                        f.next(),
                    )
                    else {
                        return Err(format!("line {n} is not a row of a record"));
                    };
                    rows.push(
                        row_of(&[f0, f1, f2, f3, f4, f5, f6, f7, f8, f9, f10])
                            .map_err(|why| format!("line {n}: {why}"))?,
                    );
                }
                Some("end") => {
                    let (Some(count), None) = (f.next(), f.next()) else {
                        return Err(format!("line {n} is not a row of a record"));
                    };
                    end = Some(number(count, "end")?);
                }
                _ => return Err(format!("line {n} is not a row of a record")),
            }
        }
        let rec = Record {
            package,
            unit,
            functions,
            rows,
        };
        match end {
            None => Err("it has no `end` line: it was cut short".into()),
            Some(e) if e != rec.total() || total != rec.total() => Err(format!(
                "it says {total} obligations (end {e}) and holds {}",
                rec.total()
            )),
            Some(_) if proved != rec.proved() => Err(format!(
                "it says {proved} proved and holds {} proved rows",
                rec.proved()
            )),
            Some(_) => Ok(rec),
        }
    }
}

/// One `o` row's eleven fields, held to their forms.
fn row_of(f: &[&str]) -> std::result::Result<Obligation, String> {
    let [
        function,
        kind,
        file,
        l0,
        c0,
        l1,
        c1,
        status,
        stop,
        anchor,
        desc,
    ] = f
    else {
        return Err("not eleven fields".into());
    };
    let n = |s: &str, what: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
            .ok_or_else(|| format!("its {what} `{s}` is not a number"))
    };
    let span = Span {
        l0: n(l0, "start line")?,
        c0: n(c0, "start column")?,
        l1: n(l1, "end line")?,
        c1: n(c1, "end column")?,
    };
    valid_function(function)?;
    valid_kind(kind)?;
    valid_file(file)?;
    valid_span(span, file)?;
    valid_status(status)?;
    valid_stop(stop)?;
    for (what, v) in [("anchor", anchor), ("description", desc)] {
        if v.is_empty() {
            return Err(format!("its {what} is empty"));
        }
    }
    Ok(Obligation {
        function: (*function).into(),
        kind: (*kind).into(),
        file: (*file).into(),
        span,
        status: (*status).into(),
        stop: (*stop).into(),
        anchor: (*anchor).into(),
        desc: (*desc).into(),
    })
}

/// A function path: non-empty, one line, no tab.
fn valid_function(s: &str) -> std::result::Result<(), String> {
    if s.is_empty() || s.chars().any(char::is_control) {
        return Err(format!("the function `{}` is no path", one_line(s)));
    }
    Ok(())
}

/// A kind: `[a-z0-9_:-]+`, and no `transport:` row (a check whose results did not reach targo).
fn valid_kind(s: &str) -> std::result::Result<(), String> {
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_:-".contains(&b))
    {
        return Err(format!(
            "the kind `{}` is of no form this gate knows",
            one_line(s)
        ));
    }
    if s.starts_with("transport:") {
        return Err(format!(
            "a `{s}` row: the check's results did not reach targo"
        ));
    }
    Ok(())
}

/// Normalized file path (`@sysroot/…`, `@registry/…`, [`NO_LOCATION`], or repo-relative).
fn valid_file(s: &str) -> std::result::Result<(), String> {
    if s == NO_LOCATION {
        return Ok(());
    }
    let rel = s
        .strip_prefix("@sysroot/")
        .or_else(|| s.strip_prefix("@registry/"))
        .unwrap_or(s);
    if rel.is_empty()
        || rel.starts_with('/')
        || rel.starts_with('@')
        || rel.chars().any(char::is_control)
        || rel
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format!(
            "the file `{}` is of no form this gate knows",
            one_line(s)
        ));
    }
    Ok(())
}

/// A relative file a report printed, its `..` folded away.
fn fold_parents(raw: &str) -> Option<String> {
    let mut kept: Vec<&str> = Vec::new();
    for c in raw.split('/') {
        match c {
            ".." => {
                kept.pop()?;
            }
            "" | "." => return None,
            c => kept.push(c),
        }
    }
    Some(kept.join("/"))
}

/// A span whose end is not before its start, on lines from 1.
fn valid_span(s: Span, file: &str) -> std::result::Result<(), String> {
    let none = Span {
        l0: 0,
        c0: 0,
        l1: 0,
        c1: 0,
    };
    if file == NO_LOCATION {
        return if s == none {
            Ok(())
        } else {
            Err(format!("an obligation with no location has the span {s}"))
        };
    }
    if s.l0 == 0 || (s.l1, s.c1) < (s.l0, s.c0) {
        return Err(format!("the span {s} ends before it starts"));
    }
    Ok(())
}

/// A status: `[a-z_]+`.
fn valid_status(s: &str) -> std::result::Result<(), String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return Err(format!(
            "the status `{}` is of no form this gate knows",
            one_line(s)
        ));
    }
    Ok(())
}

/// A stop: [`NO_STOP`], or the word of a `[stop:…]` tag (`[a-z-]+`).
fn valid_stop(s: &str) -> std::result::Result<(), String> {
    if s != NO_STOP && (s.is_empty() || !s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')) {
        return Err(format!(
            "the stop `{}` is of no form this gate knows",
            one_line(s)
        ));
    }
    Ok(())
}

/// One line of `report.ndjson`, only what the record keeps.
#[derive(Deserialize)]
struct RawRecord {
    record_type: String,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    crate_name: Option<String>,
    #[serde(default)]
    function: Option<String>,
    #[serde(default)]
    summary: Option<RawSummary>,
    #[serde(default)]
    obligations: Option<Vec<RawObligation>>,
    #[serde(default)]
    functions_emitted: Option<u64>,
}

#[derive(Deserialize)]
struct RawSummary {
    #[serde(default)]
    total_obligations: Option<u64>,
    #[serde(default)]
    proved: Option<u64>,
    #[serde(default)]
    total_proved: Option<u64>,
}

#[derive(Deserialize)]
struct RawObligation {
    kind: String,
    description: String,
    #[serde(default)]
    location: Option<RawLocation>,
    outcome: aterm_json::Value,
}

#[derive(Deserialize)]
struct RawLocation {
    file: String,
    line_start: u32,
    col_start: u32,
    line_end: u32,
    col_end: u32,
}

/// An obligation's `outcome`: its `status`, and the text that says why it stopped (`note`,
/// `reason`, …), searched whole for `[stop:…]`.
struct RawOutcome {
    status: String,
    all: aterm_json::Value,
}

impl RawOutcome {
    fn of(v: aterm_json::Value) -> std::result::Result<RawOutcome, String> {
        let status = v
            .get("status")
            .and_then(aterm_json::Value::as_str)
            .ok_or("an outcome with no status")?
            .to_string();
        Ok(RawOutcome { status, all: v })
    }
}

/// What stopped an obligation: the `[stop:<word>]` tag its outcome's text carries (a backstop's
/// first, when it carries several), `wall-backstop` for a `timeout` that names none (a
/// per-obligation clock ran out), else [`NO_STOP`].
fn stop_of(o: &RawOutcome) -> String {
    fn tags<'a>(v: &'a aterm_json::Value, out: &mut Vec<&'a str>) {
        match v {
            aterm_json::Value::String(s) => {
                let mut rest = s.as_str();
                while let Some(at) = rest.find("[stop:") {
                    rest = &rest[at + "[stop:".len()..];
                    let n = rest
                        .bytes()
                        .take_while(|b| b.is_ascii_lowercase() || *b == b'-')
                        .count();
                    if n > 0 && rest.as_bytes().get(n) == Some(&b']') {
                        out.push(&rest[..n]);
                    }
                }
            }
            aterm_json::Value::Array(a) => a.iter().for_each(|v| tags(v, out)),
            aterm_json::Value::Object(m) => m.values().for_each(|v| tags(v, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    tags(&o.all, &mut found);
    if let Some(b) = found.iter().copied().find(|t| is_backstop(t)) {
        return b.to_string();
    }
    match found.pop() {
        Some(t) => t.to_string(),
        None if o.status == "timeout" => "wall-backstop".into(),
        None => NO_STOP.into(),
    }
}

/// The `package="…"` of a report's `cargo-target(…)` unit.
fn package_of(unit: &str) -> Option<&str> {
    if !unit.starts_with("cargo-target(") || !unit.ends_with(')') {
        return None;
    }
    let at = unit.find("package=\"")? + "package=\"".len();
    let len = unit[at..].find('"')?;
    Some(&unit[at..at + len])
}

/// The sources a check's spans point into, read once each.
struct Sources<'a> {
    repo: &'a Path,
    /// The repository as the file system names it (cargo may print either).
    repo_real: Option<PathBuf>,
    /// Repo-relative directories a relative path may be relative to.
    bases: Vec<String>,
    resolved: BTreeMap<String, (String, bool)>,
    lines: BTreeMap<String, Vec<String>>,
}

impl<'a> Sources<'a> {
    fn new(repo: &'a Path, manifest: Option<&str>) -> std::result::Result<Sources<'a>, String> {
        let mut bases = Vec::new();
        if let Some(m) = manifest {
            let rel = if Path::new(m).is_absolute() {
                Path::new(m)
                    .strip_prefix(repo)
                    .map_err(|_| {
                        format!("the manifest {m} is not in the checkout {}", repo.display())
                    })?
                    .to_string_lossy()
                    .into_owned()
            } else {
                m.to_string()
            };
            let mut dir = Path::new(&rel).parent().map(Path::to_path_buf);
            while let Some(d) = dir {
                let s = d.to_string_lossy().into_owned();
                if s.is_empty() {
                    break;
                }
                bases.push(s);
                dir = d.parent().map(Path::to_path_buf);
            }
        }
        bases.push(String::new());
        Ok(Sources {
            repo,
            repo_real: fs::canonicalize(repo).ok(),
            bases,
            resolved: BTreeMap::new(),
            lines: BTreeMap::new(),
        })
    }

    /// The normalised name of a file a report printed, and whether it is in the checkout.
    fn resolve(&mut self, raw: &str) -> std::result::Result<(String, bool), String> {
        if let Some(cached) = self.resolved.get(raw) {
            return Ok(cached.clone());
        }
        let found = self.resolve_uncached(raw)?;
        self.resolved.insert(raw.to_string(), found.clone());
        Ok(found)
    }

    fn resolve_uncached(&self, raw: &str) -> std::result::Result<(String, bool), String> {
        if raw.is_empty() || raw.chars().any(char::is_control) {
            return Err(format!("a location in no file (`{}`)", one_line(raw)));
        }
        if raw.starts_with('/') {
            for root in std::iter::once(self.repo).chain(self.repo_real.as_deref()) {
                if let Ok(rest) = Path::new(raw).strip_prefix(root) {
                    let rel = rest.to_string_lossy().into_owned();
                    valid_file(&rel)?;
                    if self.repo.join(&rel).is_file() {
                        return Ok((rel, true));
                    }
                    return Err(format!("{raw} names no file in the checkout"));
                }
            }
            if let Some(rest) = sysroot_tail(raw) {
                let f = format!("@sysroot/{rest}");
                valid_file(&f)?;
                return Ok((f, false));
            }
            if let Some(at) = raw.find("/registry/src/") {
                let rest = &raw[at + "/registry/src/".len()..];
                if let Some((_index, rest)) = rest.split_once('/') {
                    let f = format!("@registry/{rest}");
                    valid_file(&f)?;
                    return Ok((f, false));
                }
            }
            return Err(format!(
                "{raw} is an absolute path of no form this gate knows (the checkout, a toolchain's library/, cargo's registry)"
            ));
        }
        let folded = fold_parents(raw)
            .ok_or_else(|| format!("{raw} is a relative path of no form this gate knows"))?;
        let raw = folded.as_str();
        for base in &self.bases {
            let rel = if base.is_empty() {
                raw.to_string()
            } else {
                format!("{base}/{raw}")
            };
            if self.repo.join(&rel).is_file() {
                return Ok((rel, true));
            }
        }
        // A toolchain library's file, as one prover names it (`core/src/…`) or another, from its
        // source tree's root (`library/core/src/…`): one name, so records compare across provers.
        let lib = raw.strip_prefix("library/").unwrap_or(raw);
        if lib
            .split('/')
            .next()
            .is_some_and(|first| SYSROOT.contains(&first))
        {
            return Ok((format!("@sysroot/{lib}"), false));
        }
        Err(format!(
            "{raw} names no file in the checkout (looked under {})",
            self.bases
                .iter()
                .map(|b| if b.is_empty() { "the root" } else { b.as_str() })
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// An in-tree file's lines, read once.
    fn lines(&mut self, rel: &str) -> std::result::Result<&[String], String> {
        if !self.lines.contains_key(rel) {
            let text = crate::durable::read_text(&self.repo.join(rel))
                .map_err(|e| format!("{rel}: {e}"))?;
            let lines = text
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect();
            self.lines.insert(rel.to_string(), lines);
        }
        Ok(self.lines.get(rel).map_or(&[], Vec::as_slice))
    }
}

/// `…/library/<sysroot crate>/<rest>` -> `<sysroot crate>/<rest>`.
fn sysroot_tail(raw: &str) -> Option<&str> {
    let mut found = None;
    let mut from = 0;
    while let Some(i) = raw[from..].find("/library/") {
        let at = from + i + "/library/".len();
        let first = raw[at..].split('/').next().unwrap_or("");
        if SYSROOT.contains(&first) && raw[at..].contains('/') {
            found = Some(&raw[at..]);
        }
        from = at;
    }
    found
}

/// The characters `a..b` of `s`.
fn chars(s: &str, a: u32, b: u32) -> Option<std::borrow::Cow<'_, str>> {
    let (a, b) = (a as usize, b as usize);
    if a > b {
        return None;
    }
    if s.is_ascii() {
        return s.get(a..b).map(std::borrow::Cow::Borrowed);
    }
    let mut it = s.chars();
    for _ in 0..a {
        it.next()?;
    }
    let need = b - a;
    let mut out = String::with_capacity(need);
    for _ in 0..need {
        out.push(it.next()?);
    }
    Some(std::borrow::Cow::Owned(out))
}

/// Whitespace collapsed to single spaces.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn visit_norm_desc<'a>(d: &'a str, mut emit: impl FnMut(&'a str)) {
    let mut first = true;
    for word in d.split_whitespace() {
        if !first {
            emit(" ");
        }
        first = false;
        let b = word.as_bytes();
        let mut i = 0;
        let mut start = 0;
        while i < b.len() {
            if b[i] == b'`' && b.get(i + 1) == Some(&b'_') {
                let digits = b[i + 2..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits > 0 && b.get(i + 2 + digits) == Some(&b'`') {
                    if start < i {
                        emit(&word[start..i]);
                    }
                    emit("`_`");
                    i += 3 + digits;
                    start = i;
                    continue;
                }
            }
            if b[i] == b'@' {
                let run = b[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .count();
                if run >= 8 {
                    if start < i {
                        emit(&word[start..i]);
                    }
                    emit("@#");
                    i += 1 + run;
                    start = i;
                    continue;
                }
            }
            i += 1;
        }
        if start < word.len() {
            emit(&word[start..]);
        }
    }
}

/// A description as [`Obligation::key`] compares it: whitespace collapsed, a MIR local
/// (`` `_7` ``) as `` `_` ``, an interned hash (`@` and 8 or more hex) as `@#`.
#[cfg(test)]
#[must_use]
pub fn norm_desc(d: &str) -> String {
    let mut out = String::with_capacity(d.len());
    visit_norm_desc(d, |chunk| out.push_str(chunk));
    out
}

/// The anchor: the source text of an in-tree span.
///
/// # Errors
/// A span past its file's lines or a line's characters.
pub fn anchor_of(lines: &[String], s: Span) -> std::result::Result<String, String> {
    let line = |i: u32| {
        lines
            .get((i as usize).wrapping_sub(1))
            .map(String::as_str)
            .ok_or_else(|| format!("line {i} is past the file's {} lines", lines.len()))
    };
    fn slice_out(
        l: &str,
        a: u32,
        b: u32,
    ) -> std::result::Result<std::borrow::Cow<'_, str>, String> {
        chars(l, a, b).ok_or_else(|| {
            format!(
                "columns {a}-{b} are past a line of {} characters",
                l.chars().count()
            )
        })
    }
    let text: std::borrow::Cow<'_, str> = if s.l0 == s.l1 {
        slice_out(line(s.l0)?, s.c0, s.c1)?
    } else {
        let first = line(s.l0)?;
        let last = line(s.l1)?;
        let len = first.chars().count() as u32;
        std::borrow::Cow::Owned(format!(
            "{} … {}",
            slice_out(first, s.c0, len)?.trim(),
            slice_out(last, 0, s.c1)?.trim()
        ))
    };
    let text = collapse(&text);
    if text.is_empty() {
        return Ok("(empty)".into());
    }
    if text.len() > 100 {
        let mut indices = text.char_indices();
        if let Some((head_end, _)) = indices.nth(80)
            && indices.nth(19).is_some()
        {
            let head = &text[..head_end];
            let digest = hex(&Sha256::digest(text.as_bytes())[..6]);
            return Ok(format!("{head}…#{digest}"));
        }
    }
    Ok(text)
}

/// Read `report.ndjson` into the check's [`Record`] ([`write_record`]).
///
/// # Errors
/// Why there is no record, in one line.
pub fn reduce(
    report: &Path,
    package: &str,
    repo: &Path,
    manifest: Option<&str>,
) -> std::result::Result<Record, String> {
    let file = crate::durable::open_regular_file(report)
        .map_err(|e| format!("{}: {e}", report.display()))?;
    let mut src = Sources::new(repo, manifest)?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut n = 0usize;
    let mut unit: Option<String> = None;
    let mut footer = false;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut rec = Record {
        package: package.to_string(),
        ..Record::default()
    };
    const MAX_REPORT_LINE_BYTES: usize = 16 * 1024 * 1024;
    loop {
        use std::io::Read as _;
        buf.clear();
        let got = (&mut reader)
            .take((MAX_REPORT_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("{}: {e}", report.display()))?;
        if got == 0 {
            break;
        }
        n += 1;
        if buf.len() > MAX_REPORT_LINE_BYTES {
            return Err(format!("line {n} exceeds {MAX_REPORT_LINE_BYTES} bytes"));
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        if footer {
            return Err(format!("line {n} follows the footer"));
        }
        if buf.is_empty() {
            return Err(format!("line {n} is empty"));
        }
        let raw: RawRecord = aterm_json::from_slice(&buf)
            .map_err(|e| format!("line {n} is not a report record: {e}"))?;
        let schema_ok = |what: &str| match raw.schema.as_deref() {
            Some(REPORT_SCHEMA) => Ok(()),
            other => Err(format!(
                "the {what} says schema {}, and this gate reads {REPORT_SCHEMA}",
                other.unwrap_or("(none)")
            )),
        };
        match (raw.record_type.as_str(), unit.as_deref()) {
            ("header", None) if n == 1 => {
                schema_ok("header")?;
                let u = raw
                    .crate_name
                    .ok_or("the header names no unit (crate_name)")?;
                if u.chars().any(char::is_control) {
                    return Err("the header's unit is not one line".into());
                }
                match package_of(&u) {
                    Some(p) if p == package => {}
                    Some(p) => {
                        return Err(format!("the report is of package {p}, not {package}"));
                    }
                    None => {
                        return Err(format!(
                            "the header's unit is no cargo-target(…) naming a package: {}",
                            one_line(&u)
                        ));
                    }
                }
                rec.unit = u.clone();
                unit = Some(u);
            }
            (t, None) => {
                return Err(format!("line {n}: a `{t}` record before the header"));
            }
            ("function", Some(u)) => {
                let full = raw.function.as_deref().unwrap_or("");
                let path = full
                    .strip_prefix(u)
                    .and_then(|r| r.strip_prefix("::"))
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "line {n}: the function `{}` is not of the form <unit>::<path>",
                            one_line(&full.chars().take(200).collect::<String>())
                        )
                    })?;
                valid_function(path).map_err(|why| format!("line {n}: {why}"))?;
                if !seen.insert(path.to_string()) {
                    return Err(format!(
                        "line {n}: {} is reported a second time, so its obligations would count twice",
                        one_line(path)
                    ));
                }
                let obligations = raw.obligations.unwrap_or_default();
                let proved = obligations
                    .iter()
                    .filter(|o| {
                        o.outcome.get("status").and_then(aterm_json::Value::as_str)
                            == Some("proved")
                    })
                    .count() as u64;
                match raw.summary {
                    Some(RawSummary {
                        total_obligations: Some(t),
                        proved: Some(p),
                        ..
                    }) if t == obligations.len() as u64 && p == proved => {}
                    _ => {
                        return Err(format!(
                            "line {n}: {path}'s summary is not its {} obligations ({proved} proved)",
                            obligations.len()
                        ));
                    }
                }
                for o in obligations {
                    let row = obligation(path, o, &mut src)
                        .map_err(|why| format!("line {n}: {path}: {why}"))?;
                    rec.rows.push(row);
                }
                rec.functions += 1;
            }
            ("footer", Some(_)) => {
                schema_ok("footer")?;
                let s = raw.summary.as_ref();
                let (t, p) = (
                    s.and_then(|s| s.total_obligations),
                    s.and_then(|s| s.total_proved),
                );
                if raw.functions_emitted != Some(rec.functions)
                    || t != Some(rec.total())
                    || p != Some(rec.proved())
                {
                    return Err(format!(
                        "the footer says {} functions, {} obligations, {} proved; the report holds {}, {}, {}",
                        raw.functions_emitted.map_or("?".into(), |v| v.to_string()),
                        t.map_or("?".into(), |v| v.to_string()),
                        p.map_or("?".into(), |v| v.to_string()),
                        rec.functions,
                        rec.total(),
                        rec.proved()
                    ));
                }
                footer = true;
            }
            ("header", Some(_)) => return Err(format!("line {n}: a second header")),
            (t, Some(_)) => {
                return Err(format!(
                    "line {n}: a `{}` record, which this gate does not read (a transport record?)",
                    one_line(t)
                ));
            }
        }
    }
    if unit.is_none() {
        return Err("the report is empty".into());
    }
    if !footer {
        return Err("the report has no footer: it was cut short".into());
    }
    Ok(rec)
}

/// A description as the record keeps it: whitespace collapsed, `-` for none.
fn kept_desc(d: &str) -> String {
    let d = collapse(d);
    if d.is_empty() { "-".into() } else { d }
}

/// One obligation of `function`, normalised.
fn obligation(
    function: &str,
    o: RawObligation,
    src: &mut Sources<'_>,
) -> std::result::Result<Obligation, String> {
    valid_kind(&o.kind)?;
    let outcome = RawOutcome::of(o.outcome)?;
    valid_status(&outcome.status)?;
    let stop = stop_of(&outcome);
    let Some(loc) = o.location else {
        return Ok(Obligation {
            function: function.to_string(),
            kind: o.kind,
            file: NO_LOCATION.into(),
            span: Span {
                l0: 0,
                c0: 0,
                l1: 0,
                c1: 0,
            },
            status: outcome.status,
            stop,
            anchor: "-".into(),
            desc: kept_desc(&o.description),
        });
    };
    let span = Span {
        l0: loc.line_start,
        c0: loc.col_start,
        l1: loc.line_end,
        c1: loc.col_end,
    };
    valid_span(span, "")?;
    let (file, in_tree) = src.resolve(&loc.file)?;
    let anchor = if in_tree {
        anchor_of(src.lines(&file)?, span).map_err(|why| format!("{file} {span}: {why}"))?
    } else {
        "-".into()
    };
    Ok(Obligation {
        function: function.to_string(),
        kind: o.kind,
        file,
        span,
        status: outcome.status,
        stop,
        anchor,
        desc: kept_desc(&o.description),
    })
}

/// [`write_record`]'s inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReduceArgs {
    pub report: PathBuf,
    pub package: String,
    pub repo: PathBuf,
    /// The check's `--manifest-path`, for a library gated by its own manifest.
    pub manifest: Option<String>,
    pub out: PathBuf,
}

/// Reduce one check's report and write its record (whole, or nothing).
///
/// # Errors
/// Why the check's report gives no record it can vouch for.
pub fn write_record(args: &ReduceArgs) -> std::result::Result<Record, String> {
    let _ = fs::remove_file(&args.out);
    reduce(
        &args.report,
        &args.package,
        &args.repo,
        args.manifest.as_deref(),
    )
    .and_then(|rec| {
        let dir = args.out.parent().unwrap_or_else(|| Path::new("."));
        let write = || -> std::io::Result<()> {
            fs::create_dir_all(dir)?;
            let stage = aterm_tempfile::Builder::new()
                .prefix(".oblig-")
                .suffix(".noindex")
                .tempdir_in(dir)?;
            let staged = stage.path().join("record.tsv");
            crate::durable::open_create_new(&staged).and_then(|mut f| {
                use std::io::Write as _;
                f.write_all(rec.to_text().as_bytes())
            })?;
            fs::rename(&staged, &args.out)
        };
        write()
            .map_err(|e| format!("{}: {e}", args.out.display()))
            .map(|()| rec)
    })
}
#[cfg(test)]
#[path = "trust_oblig_tests.rs"]
mod tests;
