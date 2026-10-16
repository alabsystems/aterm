// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per obligation: what the candidate gate ([`crate::trust_compare`]) compares when it
//! holds a candidate toolchain to the published seal.
//!
//! Not a library's count: the seal issues kernel-certified false proofs, and the fixes that
//! retract them make a correct candidate prove fewer obligations, which a count rule refuses
//! for ever; a count also hides a loss behind a gain. So each obligation the seal proved is
//! compared, and a loss is excused only by an audited row of the retraction ledger
//! ([`LEDGER_PATH`], [`Ledger`]).
//!
//! The obligations come from `report.ndjson` ([`REPORT_SCHEMA`], written by `targo trust
//! check --report-dir`), never the terminal log: its rows name no function (two functions
//! can print identical rows with opposite verdicts), drop columns, and print the standard
//! library's paths one way under the seal and another under a scratch stage2. A report runs
//! to about 150 KB an obligation, so the lane ([`write_record`]) reduces it at once to a
//! [`Record`] (`<spec>.log.oblig.tsv`), whose counts must be the report footer's and, when
//! compared, the log Summary's, and deletes it. [`reduce`] normalises each obligation's
//! function, file ([`Obligation::file`]), description ([`norm_desc`]) and anchor
//! ([`anchor_of`]).
//!
//! - The exact key ([`Exact`]) compares the seal's run with the candidate's on one tree;
//!   an out-of-tree site drops its position, which differs between two standard
//!   libraries. Losses are never summed against gains.
//! - Twins share an exact key: `(vec![0u8; x], vec![0u8; y])` reports both allocations at
//!   one `alloc/src/macros.rs` span, one proved and one failed, and a count per key would
//!   pass a candidate that swaps them. Twins are told apart by what else both runs report
//!   of them ([`Obligation::twin`]); twins still not told apart are one cell, held to its
//!   worst case ([`cell_lost`]).
//! - The ledger key ([`Key`]) names a seal obligation by its rank among look-alikes
//!   (`nth/of`), which holds only in the tree it was audited in: an edit that keeps the
//!   group's size can hand the row to an obligation nobody audited. So a row also records
//!   the aterm commit, the seal and the obligation's site at the audit, and applies only
//!   where both runs measured that commit's tree but for the ledger
//!   ([`AtermRepo::audited_tree`]), the base run is that seal, and the obligation is at
//!   that site; anywhere else it is NOT-APPLIED. The whole tree, not one file: an
//!   obligation in a macro's body, an inlined callee or the standard library sits in a
//!   file its function's edit leaves alone.
//!
//! Never the report's `obligation_id` (`vc:<fn>:<category>:<ordinal>`): any added or
//! dropped obligation renumbers its function's ordinals. Never
//! `target/trust-cache/verification.json`: it holds results from before the publication
//! boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;
use serde::Deserialize;

use crate::ledger::GitRunner;
use crate::trust_facts::{hex, one_line};

/// The record's first line.
pub const RECORD_MAGIC: &str = "# aterm trust obligations 1";
/// The one report schema [`reduce`] reads.
pub const REPORT_SCHEMA: &str = "trust.report.ndjson.v2";
/// The retraction ledger's first line (v3: each row records the seal's site of
/// its obligation, the aterm commit and the seal it was audited at).
pub const LEDGER_MAGIC: &str = "# aterm trust-gate retractions v3";
/// The first line of the ledger format before it, refused with its remedy
/// ([`parse_ledger`]).
const LEDGER_V2_MAGIC: &str = "# aterm trust-gate retractions v2";
/// Where the ledger is committed in the aterm tree: the one path a row's
/// audited tree may differ in from the tree measured (adding the row changes it).
pub const LEDGER_PATH: &str = "crates/aterm-release/trust/retractions.tsv";
/// The file of an obligation the report gives no location.
pub const NO_LOCATION: &str = "@none";
/// What the record of `<spec>.log` is called, beside it.
pub const RECORD_SUFFIX: &str = ".oblig.tsv";
/// Why the lane's reduction made no record, beside the log.
pub const ERROR_SUFFIX: &str = ".oblig.err";

/// The standard library's crates, as a toolchain prints their sources: under
/// `library/` in a checkout of trust, remapped to `<crate>/src/…` in a seal.
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

/// Where an obligation is, as the report gives it: 1-based lines, 0-based character (not
/// byte) columns, the end exclusive.
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
    /// The span's source text ([`anchor_of`]); `-` out of the tree.
    pub anchor: String,
    /// The report's description, whitespace collapsed: the keys compare it
    /// normalised ([`Obligation::norm`]), and what normalising erases tells
    /// twins apart ([`Obligation::twin`]).
    pub desc: String,
}

impl Obligation {
    /// The description as the keys compare it ([`norm_desc`]).
    #[must_use]
    pub fn norm(&self) -> String {
        norm_desc(&self.desc)
    }

    /// What tells it from a twin at its exact key: its position out of the tree (the exact
    /// key drops it), and the MIR locals and hashes its description names.
    #[must_use]
    pub fn twin(&self) -> (Option<Span>, String) {
        (
            (!self.in_tree()).then_some(self.span),
            erase(&collapse(&self.desc)).1.join(","),
        )
    }

    /// In the checkout (not the standard library's or a registry crate's).
    #[must_use]
    pub fn in_tree(&self) -> bool {
        !self.file.starts_with('@')
    }

    /// Where it is, as the exact key says it: in the tree with its position,
    /// out of it without.
    #[must_use]
    pub fn site(&self) -> String {
        if self.in_tree() {
            format!("{}:{}", self.file, self.span)
        } else {
            self.file.clone()
        }
    }

    /// Where it is, with its position in or out of the tree (`@none:0:0-0:0`
    /// with no location): what a ledger row's site records.
    #[must_use]
    pub fn at(&self) -> String {
        format!("{}:{}", self.file, self.span)
    }

    #[must_use]
    pub fn proved(&self) -> bool {
        self.status == "proved"
    }

    #[must_use]
    pub fn exact(&self) -> Exact {
        Exact {
            function: self.function.clone(),
            kind: self.kind.clone(),
            site: self.site(),
            desc: self.norm(),
        }
    }

    /// The ledger key without its `nth` and `of`.
    fn group(&self) -> (String, String, String, String, String) {
        (
            self.function.clone(),
            self.kind.clone(),
            self.file.clone(),
            self.anchor.clone(),
            self.norm(),
        )
    }
}

/// The key the seal's run and the candidate's are compared by (same tree).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Exact {
    pub function: String,
    pub kind: String,
    pub site: String,
    pub desc: String,
}

/// The key a retraction names a seal obligation by: robust to unrelated line
/// shifts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub function: String,
    pub kind: String,
    pub file: String,
    pub anchor: String,
    /// Its place, by position, among the seal's obligations sharing every other
    /// field…
    pub nth: u64,
    /// …and how many share them.
    pub of: u64,
    pub desc: String,
}

impl Key {
    /// The ledger's first seven columns, after the library.
    #[must_use]
    pub fn columns(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}/{}\t{}",
            self.function, self.kind, self.file, self.anchor, self.nth, self.of, self.desc
        )
    }

    /// One line, for a report.
    #[must_use]
    pub fn said(&self) -> String {
        format!(
            "{} | {} | {} | `{}` #{}/{} | {}",
            self.function, self.kind, self.file, self.anchor, self.nth, self.of, self.desc
        )
    }

    /// The same obligation's key but for `of`: what a ledger row is found by.
    fn names(&self, other: &Key) -> bool {
        (
            &self.function,
            &self.kind,
            &self.file,
            &self.anchor,
            self.nth,
            &self.desc,
        ) == (
            &other.function,
            &other.kind,
            &other.file,
            &other.anchor,
            other.nth,
            &other.desc,
        )
    }
}

// ---------------------------------------------------------------------------
// The record, `<spec>.log.oblig.tsv`
// ---------------------------------------------------------------------------

/// One check's obligations, as the lane recorded them beside its log —
/// tab-separated lines:
///
/// ```text
/// # aterm trust obligations 1
/// package    <the library>
/// unit       <the report's cargo-target(…) unit>
/// functions  <function records read>
/// total      <obligations>
/// proved     <obligations proved>
/// o  <function> <kind> <file> <l0> <c0> <l1> <c1> <status> <anchor> <description>
/// …
/// end        <obligations>
/// ```
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
    pub fn total(&self) -> u64 {
        self.rows.len() as u64
    }

    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = format!(
            "{RECORD_MAGIC}\npackage\t{}\nunit\t{}\nfunctions\t{}\ntotal\t{}\nproved\t{}\n",
            self.package,
            self.unit,
            self.functions,
            self.total(),
            self.proved()
        );
        for o in &self.rows {
            out.push_str(&format!(
                "o\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                o.function,
                o.kind,
                o.file,
                o.span.l0,
                o.span.c0,
                o.span.l1,
                o.span.c1,
                o.status,
                o.anchor,
                o.desc
            ));
        }
        out.push_str(&format!("end\t{}\n", self.total()));
        out
    }

    /// Read a record, held to its shape: every field in its form, the counts
    /// the rows', and an `end` line (a record cut short has none).
    ///
    /// # Errors
    /// Why the record cannot be used, in one line.
    pub fn parse(text: &str) -> std::result::Result<Record, String> {
        let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));
        match lines.next() {
            Some((_, l)) if l == RECORD_MAGIC => {}
            _ => return Err(format!("its first line is not `{RECORD_MAGIC}`")),
        }
        let mut head = |want: &str| -> std::result::Result<String, String> {
            match lines.next() {
                Some((_, l)) => match l.split_once('\t') {
                    Some((k, v)) if k == want && !v.is_empty() && !v.contains('\t') => {
                        Ok(v.to_string())
                    }
                    _ => Err(format!("its `{want}` line is missing or malformed")),
                },
                None => Err(format!("it ends before its `{want}` line")),
            }
        };
        let package = head("package")?;
        let unit = head("unit")?;
        let number = |s: String, what: &str| {
            s.parse::<u64>()
                .map_err(|_| format!("its `{what}` line is not a count"))
        };
        let functions = number(head("functions")?, "functions")?;
        let total = number(head("total")?, "total")?;
        let proved = number(head("proved")?, "proved")?;
        let mut rows = Vec::new();
        let mut end = None;
        for (n, line) in lines {
            if end.is_some() {
                return Err(format!("line {n} follows its `end` line"));
            }
            let f: Vec<&str> = line.split('\t').collect();
            match f.first().copied() {
                Some("o") if f.len() == 11 => {
                    rows.push(row_of(&f[1..]).map_err(|why| format!("line {n}: {why}"))?);
                }
                Some("end") if f.len() == 2 => {
                    end = Some(number(f[1].to_string(), "end")?);
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

/// One `o` row's ten fields, held to their forms.
fn row_of(f: &[&str]) -> std::result::Result<Obligation, String> {
    let [function, kind, file, l0, c0, l1, c1, status, anchor, desc] = f else {
        return Err("not ten fields".into());
    };
    let n = |s: &str, what: &str| {
        s.parse::<u32>()
            .map_err(|_| format!("its {what} `{s}` is not a number"))
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

/// A kind: `[a-z0-9_:-]+`, and no `transport:` row (a check whose results did
/// not reach targo).
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

/// A normalised file: `@sysroot/…`, `@registry/…`, [`NO_LOCATION`], or
/// repo-relative with no `.`, `..` or empty component.
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

/// A span whose end is not before its start, on lines from 1 — or, for an
/// obligation with no location, none (`0:0-0:0`).
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

// ---------------------------------------------------------------------------
// The reduction: report.ndjson -> the record ([`write_record`])
// ---------------------------------------------------------------------------

/// One line of `report.ndjson`, only what the record keeps: serde skips the
/// rest (the proof and transport evidence are most of each line) unread into
/// memory.
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
    outcome: RawOutcome,
}

#[derive(Deserialize)]
struct RawLocation {
    file: String,
    line_start: u32,
    col_start: u32,
    line_end: u32,
    col_end: u32,
}

#[derive(Deserialize)]
struct RawOutcome {
    status: String,
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
    /// Repo-relative directories a relative path may be relative to: the
    /// check's manifest's directory and its ancestors, deepest first (a fork
    /// prints paths relative to its own workspace's root), then the root.
    bases: Vec<String>,
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
            lines: BTreeMap::new(),
        })
    }

    /// The normalised name of a file a report printed, and whether it is in
    /// the checkout.
    fn resolve(&self, raw: &str) -> std::result::Result<(String, bool), String> {
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
        if raw
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(format!(
                "{raw} is a relative path of no form this gate knows"
            ));
        }
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
        if raw
            .split('/')
            .next()
            .is_some_and(|first| SYSROOT.contains(&first))
        {
            return Ok((format!("@sysroot/{raw}"), false));
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
            let text =
                fs::read_to_string(self.repo.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
            let lines = text
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect();
            self.lines.insert(rel.to_string(), lines);
        }
        Ok(self.lines.get(rel).map_or(&[], Vec::as_slice))
    }
}

/// `…/library/<sysroot crate>/<rest>` -> `<sysroot crate>/<rest>`, at the
/// last `library/` that names one.
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
fn chars(s: &str, a: u32, b: u32) -> Option<String> {
    let n = s.chars().count();
    let (a, b) = (a as usize, b as usize);
    (a <= b && b <= n).then(|| s.chars().skip(a).take(b - a).collect())
}

/// Whitespace collapsed to single spaces.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The anchor: the source text of an in-tree span. On one line the span's characters; over
/// several, the first line's from the start column and the last's to the end column, joined
/// ` … `; whitespace collapsed; over 100 characters, its first 80, `…#` and 12 hex of the
/// whole text's sha256.
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
    let out = |l: &str, a: u32, b: u32| {
        chars(l, a, b).ok_or_else(|| {
            format!(
                "columns {a}-{b} are past a line of {} characters",
                l.chars().count()
            )
        })
    };
    let text = if s.l0 == s.l1 {
        out(line(s.l0)?, s.c0, s.c1)?
    } else {
        let first = line(s.l0)?;
        let last = line(s.l1)?;
        let len = first.chars().count() as u32;
        format!(
            "{} … {}",
            out(first, s.c0, len)?.trim(),
            out(last, 0, s.c1)?.trim()
        )
    };
    let text = collapse(&text);
    if text.is_empty() {
        return Ok("(empty)".into());
    }
    if text.chars().count() > 100 {
        let head: String = text.chars().take(80).collect();
        let digest = hex(&Sha256::digest(text.as_bytes()));
        return Ok(format!("{head}…#{}", &digest[..12]));
    }
    Ok(text)
}

/// A description as the keys compare it: whitespace collapsed, a MIR local
/// (`` `_7` ``) as `` `_` ``, an interned hash (`@` and 8 or more hex) as `@#`.
/// Idempotent, so a record that kept it normalised compares alike.
#[must_use]
pub fn norm_desc(d: &str) -> String {
    erase(&collapse(d)).0
}

/// `d` (collapsed) normalised, and what normalising erased, in order: each MIR
/// local (`_7`) and interned hash (`@f3f39371aeb0`).
fn erase(d: &str) -> (String, Vec<String>) {
    let b = d.as_bytes();
    let mut out = String::with_capacity(d.len());
    let mut erased = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // `_<digits>`
        if b[i] == b'`' && b.get(i + 1) == Some(&b'_') {
            let digits = b[i + 2..].iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && b.get(i + 2 + digits) == Some(&b'`') {
                out.push_str("`_`");
                erased.push(d[i + 1..i + 2 + digits].to_string());
                i += 3 + digits;
                continue;
            }
        }
        // @<hex, 8 or more>
        if b[i] == b'@' {
            let run = b[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if run >= 8 {
                out.push_str("@#");
                erased.push(d[i..=i + run].to_string());
                i += 1 + run;
                continue;
            }
        }
        // Copy one whole character (the markers above are ASCII).
        let ch = d[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    if out.is_empty() {
        out.push('-');
    }
    (out, erased)
}

/// Reads `report.ndjson` into the check's [`Record`] ([`write_record`] runs it after each
/// check of the lane), or says why not, and the comparison refuses that side.
///
/// It refuses what the record could not vouch for: a line that is not a record, a header
/// not first or of another schema or package, a record type it does not read (transport),
/// a function outside the unit or reported twice (same-name methods of two impl blocks
/// report distinct paths such as `W::<u8>::inc`), a kind, status, path or span of no form
/// it knows or past its source, counts that are not the rows', and a report with no footer
/// (cut short).
///
/// # Errors
/// Why there is no record, in one line.
pub fn reduce(
    report: &Path,
    package: &str,
    repo: &Path,
    manifest: Option<&str>,
) -> std::result::Result<Record, String> {
    let file = fs::File::open(report).map_err(|e| format!("{}: {e}", report.display()))?;
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
    loop {
        buf.clear();
        let got = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("{}: {e}", report.display()))?;
        if got == 0 {
            break;
        }
        n += 1;
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
                    .filter(|o| o.outcome.status == "proved")
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

/// A description as the record keeps it: whitespace collapsed (a record row
/// is one line of tab-separated fields), `-` for none.
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
    valid_status(&o.outcome.status)?;
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
            status: o.outcome.status,
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
    // A reported location is held to the ordinary rule, whatever it names.
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
        status: o.outcome.status,
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
    // A record an earlier run left must not read as this one's.
    let _ = fs::remove_file(&args.out);
    reduce(
        &args.report,
        &args.package,
        &args.repo,
        args.manifest.as_deref(),
    )
    .and_then(|rec| {
        let tmp = args.out.with_extension("tsv.tmp");
        fs::write(&tmp, rec.to_text())
            .and_then(|()| fs::rename(&tmp, &args.out))
            .map_err(|e| format!("{}: {e}", args.out.display()))
            .map(|()| rec)
    })
}

// ---------------------------------------------------------------------------
// The retraction ledger, `crates/aterm-release/trust/retractions.tsv`
// ---------------------------------------------------------------------------

/// One audited retraction: a seal obligation (by its ledger [`Key`], of the seal and aterm
/// tree it was audited against, at the site it was audited at) that a trust commit stops
/// proving because the seal's proof of it was false.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retraction {
    /// Its line in the ledger.
    pub line: usize,
    pub library: String,
    pub key: Key,
    /// The published seal's position of the obligation when it was audited, in
    /// the key's file.
    pub at: Span,
    /// The aterm commit it was audited at (40 hex): the commit both runs
    /// measured then.
    pub aterm: String,
    /// The published seal's trust commit it was audited against (40 hex):
    /// another seal's record may order look-alikes at one site otherwise.
    pub seal: String,
    /// The trust superproject commit that retracts it (40 hex).
    pub commit: String,
    /// The trust test reproducing the false proof: a path in trust, and the
    /// test fn's name.
    pub test_path: String,
    pub test_fn: String,
    /// Who reviewed it, when, and which false-proof class.
    pub reviewed: String,
}

impl Retraction {
    #[must_use]
    pub fn reproducer(&self) -> String {
        format!("{}::{}", self.test_path, self.test_fn)
    }

    /// The site it was audited at, as its ledger row spells it.
    #[must_use]
    pub fn site(&self) -> String {
        format!("{}:{}", self.key.file, self.at)
    }
}

/// The ledger as read: its rows and why any line could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    pub path: String,
    pub rows: Vec<Retraction>,
    pub errors: Vec<String>,
}

/// A Rust identifier.
fn ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next()
        .is_some_and(|f| f == '_' || f.is_ascii_alphabetic())
        && c.all(|x| x == '_' || x.is_ascii_alphanumeric())
}

/// 40 lowercase hex.
#[must_use]
pub fn full_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A ledger row's `<place>/<of>`: positive whole numbers of at most nine
/// digits with no leading zero, the place no more than the count.
fn nth_of(s: &str) -> Option<(u64, u64)> {
    let whole = |t: &str| {
        (!t.is_empty()
            && t.len() <= 9
            && t.bytes().all(|b| b.is_ascii_digit())
            && !t.starts_with('0'))
        .then(|| t.parse::<u64>().ok())
        .flatten()
    };
    let (a, b) = s.split_once('/')?;
    let (place, of) = (whole(a)?, whole(b)?);
    (place <= of).then_some((place, of))
}

/// A ledger row's site, `<file>:<l0>:<c0>-<l1>:<c1>` with `file` its own
/// file column: whole numbers of at most nine digits with no leading zero,
/// held to the record's rule for a span of that file ([`valid_span`]).
fn site_of(s: &str, file: &str) -> Option<Span> {
    let whole = |t: &str| {
        (!t.is_empty()
            && t.len() <= 9
            && t.bytes().all(|b| b.is_ascii_digit())
            && (t == "0" || !t.starts_with('0')))
        .then(|| t.parse::<u32>().ok())
        .flatten()
    };
    let rest = s.strip_prefix(file)?.strip_prefix(':')?;
    let (from, to) = rest.split_once('-')?;
    let ((l0, c0), (l1, c1)) = (from.split_once(':')?, to.split_once(':')?);
    let span = Span {
        l0: whole(l0)?,
        c0: whole(c0)?,
        l1: whole(l1)?,
        c1: whole(c1)?,
    };
    valid_span(span, file).ok().map(|()| span)
}

/// Read the ledger: [`LEDGER_MAGIC`], `#` comments, and rows of exactly thirteen
/// tab-separated, non-empty fields: library, function, kind, file, anchor, nth
/// (`<place>/<of>`), description, site (the seal's `<file>:<l0>:<c0>-<l1>:<c1>` of the
/// obligation when audited), aterm commit and seal (the published seal's trust commit) it
/// was audited at, the trust commit that retracts it, reproducer, reviewed. A row of a
/// library neither plan names, two rows naming one obligation (whatever their `of`), or a
/// field out of its form is an error, and no comparison is made on an erroneous ledger. A
/// v2 ledger is refused whole, with its remedy.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_ledger(text: &str, path: &str, libraries: &BTreeSet<String>) -> Ledger {
    let mut out = Ledger {
        path: path.to_string(),
        ..Ledger::default()
    };
    let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));
    match lines.next().map(|(_, l)| l) {
        Some(LEDGER_MAGIC) => {}
        Some(LEDGER_V2_MAGIC) => {
            out.errors.push(format!(
                "{path}: first line is `{LEDGER_V2_MAGIC}`, not `{LEDGER_MAGIC}`: a v2 row names its obligation only by its place among look-alikes, which any edit to the tree can reassign, so no v2 row is read. Re-audit each row on the measured runs, add columns 8-10 (the published seal's --obligations-tsv `site`, `aterm` and `seal`), and make the first line `{LEDGER_MAGIC}`"
            ));
            return out;
        }
        _ => {
            out.errors.push(format!(
                "{path}: its first line is not `{LEDGER_MAGIC}`, so it is no retraction ledger this gate reads"
            ));
            return out;
        }
    }
    let mut seen = BTreeSet::new();
    for (n, line) in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: String| format!("{path} line {n}: {why}");
        let f: Vec<&str> = line.split('\t').collect();
        let [
            library,
            function,
            kind,
            file,
            anchor,
            nth,
            desc,
            site,
            aterm,
            seal,
            commit,
            reproducer,
            reviewed,
        ] = f[..]
        else {
            out.errors.push(bad(format!(
                "{} fields, and a retraction has exactly 13",
                f.len()
            )));
            continue;
        };
        if let Some(empty) = f.iter().position(|v| v.is_empty() || v.trim() != *v) {
            out.errors.push(bad(format!(
                "field {} is empty or padded with whitespace",
                empty + 1
            )));
            continue;
        }
        let checks = [
            valid_function(function),
            valid_kind(kind),
            valid_file(file),
            nth_of(nth).map(|_| ()).ok_or_else(|| {
                format!(
                    "nth `{nth}` is not <place>/<of>: two positive whole numbers, the place no more than the count"
                )
            }),
            site_of(site, file).map(|_| ()).ok_or_else(|| {
                format!(
                    "the site `{site}` is not <its file>:<l0>:<c0>-<l1>:<c1>, a span of {file}: where the published seal reported the obligation when it was audited"
                )
            }),
            full_sha(aterm).then_some(()).ok_or_else(|| {
                format!("the aterm commit `{aterm}` is not 40 lowercase hex")
            }),
            full_sha(seal).then_some(()).ok_or_else(|| {
                format!("the seal's trust commit `{seal}` is not 40 lowercase hex")
            }),
            full_sha(commit)
                .then_some(())
                .ok_or_else(|| format!("the trust commit `{commit}` is not 40 lowercase hex")),
            libraries
                .contains(library)
                .then_some(())
                .ok_or_else(|| format!("the library {library} is in neither run's plan")),
        ];
        if let Some(why) = checks.into_iter().find_map(std::result::Result::err) {
            out.errors.push(bad(why));
            continue;
        }
        let Some((test_path, test_fn)) = reproducer.rsplit_once("::").filter(|(p, t)| {
            ident(t)
                && !p.is_empty()
                && !p.starts_with('/')
                && !p.contains(char::is_whitespace)
                && !p.split('/').any(|c| c.is_empty() || c == "." || c == "..")
        }) else {
            out.errors.push(bad(format!(
                "the reproducer `{reproducer}` is not <path in trust>::<test fn>"
            )));
            continue;
        };
        let (place, of) = nth_of(nth).unwrap_or((0, 0));
        // Held to its form above; an error, never a row dropped, if not.
        let Some(at) = site_of(site, file) else {
            out.errors
                .push(bad(format!("the site `{site}` does not read")));
            continue;
        };
        let key = Key {
            function: function.into(),
            kind: kind.into(),
            file: file.into(),
            anchor: anchor.into(),
            nth: place,
            of,
            desc: desc.into(),
        };
        let named = Key {
            of: 0,
            ..key.clone()
        };
        if !seen.insert((library.to_string(), named)) {
            out.errors.push(bad(
                "the same obligation (library, key and nth) as an earlier row".into(),
            ));
            continue;
        }
        out.rows.push(Retraction {
            line: n,
            library: library.into(),
            key,
            at,
            aterm: aterm.into(),
            seal: seal.into(),
            commit: commit.into(),
            test_path: test_path.into(),
            test_fn: test_fn.into(),
            reviewed: reviewed.into(),
        });
    }
    out
}

/// Whether the candidate's trust holds a retraction's fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Contains {
    Yes,
    /// It does not, and why: its commit is no ancestor of the candidate's, a
    /// commit between them reverts it, or the reproducer is gone from the
    /// candidate's tree.
    No(String),
    /// It could not be told, and why: no retraction applies.
    Unknown(String),
}

/// The global options every git the gate runs in the trust repository (and the
/// aterm one) takes: commits, their ancestry and their trees as the objects
/// record them, never as a replace ref (`git replace --graft`) or a cache says.
const TRUST_GIT: [&str; 3] = ["--no-replace-objects", "-c", "core.commitGraph=false"];

/// `git` through `runner`, with [`TRUST_GIT`].
fn hardened(
    runner: &dyn GitRunner,
    args: &[&str],
) -> std::result::Result<crate::ledger::RunOut, String> {
    let mut all: Vec<&str> = TRUST_GIT.to_vec();
    all.extend_from_slice(args);
    runner.git(&all).map_err(|e| e.to_string())
}

/// `git <what>` in `shown` failed with `out`, as one line.
fn git_failed(shown: &str, what: &str, out: &crate::ledger::RunOut) -> String {
    format!(
        "git {what} in {shown} exited {}: {}",
        out.status,
        one_line(out.stderr_utf8().trim())
    )
}

/// The trust repository the ledger's commits are read in.
pub struct TrustRepo<'a> {
    pub git: &'a dyn GitRunner,
    pub shown: String,
}

impl TrustRepo<'_> {
    /// git there, with [`TRUST_GIT`].
    fn run(&self, args: &[&str]) -> std::result::Result<crate::ledger::RunOut, String> {
        hardened(self.git, args)
    }

    /// `git <what>` in the repository failed with `out`, as one line.
    fn failed(&self, what: &str, out: &crate::ledger::RunOut) -> String {
        git_failed(&self.shown, what, out)
    }

    /// Why ancestry cannot be read here, if it cannot: a shallow repository (its cut
    /// history reads a fix a depth-1 seal holds as not in it), or a graft file, which
    /// rewrites parents.
    #[must_use]
    pub fn unfit(&self) -> Option<String> {
        match self.run(&["rev-parse", "--is-shallow-repository"]) {
            Ok(out) if out.success() && out.stdout_utf8().trim() == "false" => {}
            Ok(out) if out.success() => {
                return Some(format!(
                    "{} is a shallow repository: its history is cut, so whether one commit contains another cannot be read there (git fetch --unshallow)",
                    self.shown
                ));
            }
            Ok(out) => return Some(self.failed("rev-parse --is-shallow-repository", &out)),
            Err(e) => return Some(e),
        }
        match self.run(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/grafts",
        ]) {
            Ok(out) if out.success() => {
                let grafts = out.stdout_utf8().trim().to_string();
                Path::new(&grafts).exists().then(|| {
                    format!(
                        "{} has a graft file ({grafts}), which rewrites what a commit's parents are: remove it",
                        self.shown
                    )
                })
            }
            Ok(out) => Some(self.failed("rev-parse --git-path info/grafts", &out)),
            Err(e) => Some(e),
        }
    }

    /// Whether `sha` is a commit there.
    ///
    /// # Errors
    /// git did not answer.
    pub fn is_commit(&self, sha: &str) -> std::result::Result<bool, String> {
        let out = self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{sha}^{{commit}}"),
        ])?;
        match out.status {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(self.failed("rev-parse", &out)),
        }
    }

    /// Whether `a` is an ancestor of (or is) `b`.
    ///
    /// # Errors
    /// git exited other than 0 or 1.
    pub fn is_ancestor(&self, a: &str, b: &str) -> std::result::Result<bool, String> {
        let out = self.run(&["merge-base", "--is-ancestor", a, b])?;
        match out.status {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(self.failed(
                &format!(
                    "merge-base --is-ancestor {} {}",
                    &a[..a.len().min(12)],
                    &b[..b.len().min(12)]
                ),
                &out,
            )),
        }
    }

    /// A commit after `fix` up to `at` that says it reverts it (`git revert`'s
    /// `This reverts commit <fix>`), if one does.
    ///
    /// # Errors
    /// git did not answer.
    pub fn reverted(&self, fix: &str, at: &str) -> std::result::Result<Option<String>, String> {
        let grep = format!("--grep=This reverts commit {fix}");
        let range = format!("{fix}..{at}");
        let out = self.run(&["rev-list", "--fixed-strings", &grep, &range])?;
        if !out.success() {
            return Err(self.failed("rev-list --grep", &out));
        }
        Ok(out.stdout_utf8().lines().next().map(str::to_string))
    }

    /// Why the reproducer is not at trust `at`, if it is not: its file in the
    /// tree of that commit, holding `fn <name>`.
    ///
    /// # Errors
    /// git did not answer.
    pub fn reproducer_missing(
        &self,
        r: &Retraction,
        at: &str,
    ) -> std::result::Result<Option<String>, String> {
        let out = self.run(&["show", &format!("{at}:{}", r.test_path)])?;
        if !out.success() {
            return Ok(Some(format!(
                "{} is not a file of trust {} ({})",
                r.test_path,
                &at[..at.len().min(12)],
                one_line(out.stderr_utf8().trim())
            )));
        }
        let text = out.stdout_utf8();
        let needle = format!("fn {}", r.test_fn);
        let found = text.match_indices(&needle).any(|(i, _)| {
            let after = text[i + needle.len()..].chars().next();
            let before = text[..i].chars().next_back();
            matches!(after, Some('(' | '<' | ' '))
                && before.is_none_or(|c| !(c == '_' || c.is_ascii_alphanumeric()))
        });
        Ok((!found).then(|| {
            format!(
                "{} at trust {} has no `fn {}`",
                r.test_path,
                &at[..at.len().min(12)],
                r.test_fn
            )
        }))
    }

    /// Whether the candidate's trust `cand` holds `r`'s fix: its commit an
    /// ancestor of `cand`, no commit between them reverting it, and its
    /// reproducer still in `cand`'s tree (a fix undone by hand takes the test
    /// that pins it with it, or trust's own tests fail on it).
    ///
    /// # Errors
    /// git did not answer.
    pub fn holds_fix(&self, r: &Retraction, cand: &str) -> std::result::Result<Contains, String> {
        let fix = &r.commit[..r.commit.len().min(12)];
        if !self.is_ancestor(&r.commit, cand)? {
            return Ok(Contains::No(format!(
                "the candidate does not contain trust {fix}"
            )));
        }
        if let Some(rev) = self.reverted(&r.commit, cand)? {
            return Ok(Contains::No(format!(
                "the candidate's trust reverts {fix} (trust {} says `This reverts commit {fix}…`)",
                &rev[..rev.len().min(12)]
            )));
        }
        if let Some(missing) = self.reproducer_missing(r, cand)? {
            return Ok(Contains::No(format!(
                "the candidate's trust no longer holds {fix}'s reproducer ({missing}), so nothing shows the fix is still in it"
            )));
        }
        Ok(Contains::Yes)
    }
}

/// Whether a retraction was audited where the comparison applies it: against
/// the published seal the base run measured with, in the aterm tree both runs
/// measured ([`AtermRepo::audited_tree`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audited {
    /// The same seal, and the same tree but for the ledger itself
    /// ([`LEDGER_PATH`]).
    Same,
    /// Another, and how: the row's key may name another obligation there.
    Changed(String),
    /// It could not be told, and why: the row does not apply.
    Unknown(String),
}

/// The aterm repository a retraction's audited commit is read in. Any
/// repository holding both commits answers alike: a commit names its tree by
/// content.
pub struct AtermRepo<'a> {
    pub git: &'a dyn GitRunner,
    pub shown: String,
}

impl AtermRepo<'_> {
    /// Whether the tree of aterm `measured` is the tree of `audited` but for the ledger
    /// ([`LEDGER_PATH`]): every other path's entry, mode and object id the same, a
    /// submodule's gitlink included whatever `.gitmodules` or config says to ignore. Read
    /// with `git diff-tree` (plumbing: no attribute, textconv or external diff), never `git
    /// diff --quiet`, which `ignore = all` or `diff.ignoreSubmodules` quiets about a
    /// submodule bump. No repository, or either commit missing from it, cannot be told.
    /// The seal is the caller's to hold to the row.
    pub fn audited_tree(
        &self,
        audited: &str,
        measured: &str,
    ) -> std::result::Result<Audited, String> {
        let short = |s: &str| s[..s.len().min(12)].to_string();
        let again = format!(
            "audit the row again at aterm {} and re-stamp it",
            short(measured)
        );
        let repo = hardened(self.git, &["rev-parse", "--git-dir"])?;
        if !repo.success() {
            return Ok(Audited::Unknown(format!(
                "{} is no git repository ({}), so whether the tree measured is the one the row was audited in cannot be told (keep the ledger in the aterm checkout, or {again})",
                self.shown,
                one_line(repo.stderr_utf8().trim())
            )));
        }
        for (sha, what) in [
            (audited, "the commit the row was audited at"),
            (measured, "the commit the runs measured"),
        ] {
            let out = hardened(
                self.git,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{sha}^{{commit}}"),
                ],
            )?;
            match out.status {
                0 => {}
                1 => {
                    return Ok(Audited::Unknown(format!(
                        "aterm {} ({what}) is not a commit of {}, so whether the tree measured is the one the row was audited in cannot be told (fetch it, or {again})",
                        short(sha),
                        self.shown
                    )));
                }
                _ => return Err(git_failed(&self.shown, "rev-parse", &out)),
            }
        }
        let out = hardened(
            self.git,
            &[
                "diff-tree",
                "-r",
                "-z",
                "--no-renames",
                "--name-only",
                "--ignore-submodules=none",
                "--no-ext-diff",
                "--no-textconv",
                audited,
                measured,
            ],
        )?;
        if !out.success() {
            return Err(git_failed(&self.shown, "diff-tree", &out));
        }
        let changed: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .filter(|p| p != LEDGER_PATH)
            .collect();
        if changed.is_empty() {
            return Ok(Audited::Same);
        }
        let named = changed
            .iter()
            .take(3)
            .map(|p| one_line(p))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Audited::Changed(format!(
            "it was audited at aterm {} and the runs measured aterm {}, whose trees differ in {} path(s) besides the ledger ({named}{}), so its key may name another obligation than the one audited — {again}",
            short(audited),
            short(measured),
            changed.len(),
            if changed.len() > 3 { ", …" } else { "" }
        )))
    }
}

/// The git the gate reads the trust repository (and [`AtermRepo`]) with:
/// [`crate::ledger::object_git`]. A graft file, which no variable turns off quietly,
/// [`TrustRepo::unfit`] refuses.
pub struct TrustGit {
    repo: PathBuf,
}

impl TrustGit {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        TrustGit { repo: repo.into() }
    }
}

impl GitRunner for TrustGit {
    fn git(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
        let out = crate::ledger::object_git(&self.repo)
            .args(args)
            .output()
            .map_err(|e| {
                crate::ledger::Error::new(format!("failed to spawn git {}: {e}", args.join(" ")))
            })?;
        Ok(crate::ledger::RunOut {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// The short hashes a driver's `--version` line names: the first parenthesis's
/// leading hex (`targo 1.99.0-dev (d36c551fa 2026-10-01)`), and a `+g<hex>`.
#[must_use]
pub fn driver_hashes(driver_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(open) = driver_id.find('(') {
        let tok: String = driver_id[open + 1..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if tok.len() >= 7 {
            out.push(tok.to_ascii_lowercase());
        }
    }
    for (i, _) in driver_id.match_indices("+g") {
        let tok: String = driver_id[i + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if tok.len() >= 7 {
            out.push(tok.to_ascii_lowercase());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// One library, the seal's record against the candidate's
// ---------------------------------------------------------------------------

/// A seal obligation the candidate does not prove, unexcused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lost {
    pub key: Key,
    /// Where the seal reported it, with its position in or out of the tree
    /// ([`Obligation::at`]): a ledger row's site.
    pub site: String,
    /// The candidate's statuses at its cell, or `MISSING`.
    pub cand: String,
    pub missing: bool,
    /// Why it is counted when it is one of twins the comparison cannot tell
    /// apart, and a retraction that names it but did not apply, and why.
    pub note: String,
}

/// A seal obligation the candidate does not prove, excused by a retraction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excused {
    pub key: Key,
    pub site: String,
    pub cand: String,
    pub line: usize,
    pub commit: String,
    pub reproducer: String,
    pub reviewed: String,
}

/// One library's per-obligation comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibCmp {
    pub lost: Vec<Lost>,
    pub excused: Vec<Excused>,
    /// Obligations the candidate proves beyond the seal — said, never set
    /// against a loss.
    pub gained: u64,
    /// Retractions of this library that excused nothing, and why: UNUSED (it names nothing
    /// the seal proved; prune it once a seal without it is published) or NOT-APPLIED (a
    /// condition [`compare_lib`] sets for an excuse fails, or cannot be told).
    pub notes: Vec<(usize, &'static str, String)>,
    /// Retractions the candidate contradicts: it holds the fix and still
    /// proves the obligation. The ledger is wrong for it; refused.
    pub stale: Vec<(usize, String)>,
}

impl LibCmp {
    /// Distinct functions among the lost.
    #[must_use]
    pub fn functions_lost(&self) -> usize {
        self.lost
            .iter()
            .map(|l| l.key.function.as_str())
            .collect::<BTreeSet<_>>()
            .len()
    }

    #[must_use]
    pub fn missing(&self) -> usize {
        self.lost.iter().filter(|l| l.missing).count()
    }
}

/// Every base row's ledger key, in row order: `nth` is the row's place, by
/// span, among the rows sharing its other fields, and `of` how many share them.
#[must_use]
pub fn ledger_keys(rec: &Record) -> Vec<Key> {
    let mut groups: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    for (i, o) in rec.rows.iter().enumerate() {
        groups.entry(o.group()).or_default().push(i);
    }
    let mut place = vec![(0u64, 0u64); rec.rows.len()];
    for idx in groups.values_mut() {
        idx.sort_by_key(|&i| (rec.rows[i].span, i));
        let of = idx.len() as u64;
        for (n, &i) in idx.iter().enumerate() {
            place[i] = (n as u64 + 1, of);
        }
    }
    rec.rows
        .iter()
        .zip(place)
        .map(|(o, (nth, of))| Key {
            function: o.function.clone(),
            kind: o.kind.clone(),
            file: o.file.clone(),
            anchor: o.anchor.clone(),
            nth,
            of,
            desc: o.norm(),
        })
        .collect()
}

/// A cell: the seal's and the candidate's obligations that nothing either report says
/// tells apart (row indices of each record).
#[derive(Clone, Debug, Default)]
struct Cell {
    base: Vec<usize>,
    cand: Vec<usize>,
}

/// The cells of two records: one per exact key — or, where a key holds more
/// than one obligation on either side and both runs report the same multiset
/// of [`Obligation::twin`] there, one per twin value. (When the two runs report
/// different ones — a standard library that moved, MIR renumbered — the key
/// stays one cell: pairing by a value that changed would pair the wrong ones.)
fn cells(base: &Record, cand: &Record) -> Vec<Cell> {
    let mut at: BTreeMap<Exact, Cell> = BTreeMap::new();
    for (i, o) in base.rows.iter().enumerate() {
        at.entry(o.exact()).or_default().base.push(i);
    }
    for (j, o) in cand.rows.iter().enumerate() {
        at.entry(o.exact()).or_default().cand.push(j);
    }
    let mut out = Vec::with_capacity(at.len());
    for cell in at.into_values() {
        if cell.base.len() <= 1 && cell.cand.len() <= 1 {
            out.push(cell);
            continue;
        }
        let twins = |rec: &Record, rows: &[usize]| {
            let mut v: Vec<_> = rows.iter().map(|&i| rec.rows[i].twin()).collect();
            v.sort();
            v
        };
        if twins(base, &cell.base) != twins(cand, &cell.cand) {
            out.push(cell);
            continue;
        }
        let mut split: BTreeMap<(Option<Span>, String), Cell> = BTreeMap::new();
        for &i in &cell.base {
            split.entry(base.rows[i].twin()).or_default().base.push(i);
        }
        for &j in &cell.cand {
            split.entry(cand.rows[j].twin()).or_default().cand.push(j);
        }
        out.extend(split.into_values());
    }
    out
}

/// The worst case of a cell of `n` seal and `m` candidate obligations, `p` and `q` of them
/// proved: any seal-proved one may be any candidate one, so as many are lost as the
/// candidate's unproved ones, and the seal's beyond the candidate's count, can hide, never
/// more than the seal proved. One a side is the ordinary case: lost iff the seal proved it
/// and the candidate does not.
#[must_use]
pub fn cell_lost(n: u64, m: u64, p: u64, q: u64) -> u64 {
    p.min(m.saturating_sub(q) + n.saturating_sub(m))
}

/// Compares one library's records: every obligation the seal proved must be proved by the
/// candidate, cell by cell at its worst ([`cell_lost`]), except what a retraction excuses:
/// one audited against this seal and aterm tree ([`Audited::Same`] from `audited`), whose
/// obligation is at the audited site in a group of the audited size, whose fix the
/// candidate holds ([`Contains::Yes`] from `contains`), and whose obligation the candidate
/// does not prove, as only a cell that tells it apart can say (one obligation a side, or
/// twins none of which the candidate proves). A seal obligation the candidate reports
/// nothing at (MISSING) no retraction excuses; gains offset nothing.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn compare_lib(
    base: &Record,
    cand: &Record,
    retractions: &[&Retraction],
    audited: &mut dyn FnMut(&Retraction) -> Audited,
    contains: &mut dyn FnMut(&Retraction) -> Contains,
) -> LibCmp {
    let keys = ledger_keys(base);
    let cells = cells(base, cand);
    let mut cell_of = vec![0usize; base.rows.len()];
    for (ci, c) in cells.iter().enumerate() {
        for &i in &c.base {
            cell_of[i] = ci;
        }
    }
    // (n, m, p, q, lost) per cell.
    let tally: Vec<(u64, u64, u64, u64, u64)> = cells
        .iter()
        .map(|c| {
            let n = c.base.len() as u64;
            let m = c.cand.len() as u64;
            let p = c.base.iter().filter(|&&i| base.rows[i].proved()).count() as u64;
            let q = c.cand.iter().filter(|&&j| cand.rows[j].proved()).count() as u64;
            (n, m, p, q, cell_lost(n, m, p, q))
        })
        .collect();
    let said = |c: &Cell| {
        if c.cand.is_empty() {
            "MISSING".to_string()
        } else {
            c.cand
                .iter()
                .map(|&j| cand.rows[j].status.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut out = LibCmp {
        gained: tally
            .iter()
            .map(|&(_, _, p, q, lost)| q.saturating_sub(p - lost))
            .sum(),
        ..LibCmp::default()
    };
    // Why a retraction naming a base row did not apply, said on its loss.
    let mut named: BTreeMap<usize, String> = BTreeMap::new();
    let mut excused: BTreeSet<usize> = BTreeSet::new();
    fn not_applied(
        out: &mut LibCmp,
        named: &mut BTreeMap<usize, String>,
        r: &Retraction,
        i: usize,
        why: String,
    ) {
        named.insert(
            i,
            format!("retraction line {} does not apply: {why}", r.line),
        );
        out.notes.push((r.line, "NOT-APPLIED", why));
    }
    for r in retractions {
        let found = keys.iter().position(|k| k.names(&r.key));
        // In any tree but the one it was audited in, or another seal's record, its rank
        // among look-alikes may name another obligation, even at the same group size and
        // site: it names nothing for certain there, so it applies to nothing.
        match audited(r) {
            Audited::Same => {}
            Audited::Changed(why) | Audited::Unknown(why) => {
                match found {
                    Some(i) => not_applied(&mut out, &mut named, r, i, why),
                    None => out.notes.push((r.line, "NOT-APPLIED", why)),
                }
                continue;
            }
        }
        let Some(i) = found else {
            out.notes.push((
                r.line,
                "UNUSED",
                "it names no obligation the published seal reported".into(),
            ));
            continue;
        };
        let o = &base.rows[i];
        if o.span != r.at {
            let why = format!(
                "the obligation its key names is at {} under the published seal, and the row was audited at {}: it names another obligation than the one audited — audit it again",
                o.at(),
                r.site()
            );
            not_applied(&mut out, &mut named, r, i, why);
            continue;
        }
        if keys[i].of != r.key.of {
            let why = format!(
                "the published seal reports {} obligations sharing its fields and the row was audited at {}: one was added or removed beside it, so its place may name another — audit it again",
                keys[i].of, r.key.of
            );
            not_applied(&mut out, &mut named, r, i, why);
            continue;
        }
        if !o.proved() {
            out.notes.push((
                r.line,
                "UNUSED",
                format!("the published seal did not prove it ({})", o.status),
            ));
            continue;
        }
        let c = &cells[cell_of[i]];
        let (n, m, _, q, _) = tally[cell_of[i]];
        if m == 0 {
            let why =
                "the candidate reports no obligation there (MISSING), which no retraction excuses"
                    .to_string();
            not_applied(&mut out, &mut named, r, i, why);
            continue;
        }
        // Whether the candidate proves this obligation: told when it proves every one
        // of the cell's (so this one too) or none of them (nor this one), with no fewer
        // than the seal reported there.
        let still = q == m && m >= n;
        let not_now = q == 0 && m >= n;
        if !still && !not_now {
            let why = format!(
                "it is one of {n} obligations the published seal reported at {} and {m} the candidate did, which nothing reported tells apart, and the candidate proves {q} of its {m}: whether it proves this one cannot be told",
                o.site()
            );
            not_applied(&mut out, &mut named, r, i, why);
            continue;
        }
        match contains(r) {
            Contains::Yes if still => out.stale.push((
                r.line,
                format!(
                    "the candidate contains trust {} and still proves {} (at {}: {})",
                    &r.commit[..r.commit.len().min(12)],
                    r.key.said(),
                    o.at(),
                    said(c)
                ),
            )),
            Contains::Yes => {
                excused.insert(i);
                out.excused.push(Excused {
                    key: keys[i].clone(),
                    site: o.at(),
                    cand: said(c),
                    line: r.line,
                    commit: r.commit.clone(),
                    reproducer: r.reproducer(),
                    reviewed: r.reviewed.clone(),
                });
            }
            Contains::No(why) | Contains::Unknown(why) => {
                not_applied(&mut out, &mut named, r, i, why);
            }
        }
    }
    for (ci, c) in cells.iter().enumerate() {
        let (n, m, p, q, lost) = tally[ci];
        if lost == 0 {
            continue;
        }
        let mut proved: Vec<usize> = c
            .base
            .iter()
            .copied()
            .filter(|&i| base.rows[i].proved())
            .collect();
        proved.sort_by_key(|&i| (keys[i].nth, i));
        let twins = (n > 1 || m > 1).then(|| {
            format!(
                "twins: {n} under the published seal and {m} under the candidate at this key, which nothing reported tells apart; the candidate proves {q} of its {m}, so {lost} of the {p} the seal proved may be lost, and each counts"
            )
        });
        let take = usize::try_from(lost).unwrap_or(usize::MAX);
        for &i in proved.iter().take(take) {
            if excused.contains(&i) {
                continue;
            }
            let note = [twins.clone(), named.get(&i).cloned()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
            out.lost.push(Lost {
                key: keys[i].clone(),
                site: base.rows[i].at(),
                cand: said(c),
                missing: m == 0,
                note,
            });
        }
    }
    out.lost
        .sort_by(|a, b| (&a.key.file, &a.site, &a.key).cmp(&(&b.key.file, &b.site, &b.key)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("trust-oblig")
            .tempdir()
            .unwrap()
    }

    #[test]
    fn descriptions_lose_mir_locals_and_hashes_and_nothing_else() {
        assert_eq!(
            norm_desc("unbounded allocation in `vec::from_elem` (count `_14`):\n  bulk"),
            "unbounded allocation in `vec::from_elem` (count `_`): bulk"
        );
        assert_eq!(
            norm_desc("for Box<Page>::new::<__trust_args@f3f39371aeb0>"),
            "for Box<Page>::new::<__trust_args@#>"
        );
        // A named local, a short hex run and a non-ASCII text are kept.
        assert_eq!(norm_desc("(count `n`) @beef — é"), "(count `n`) @beef — é");
        assert_eq!(norm_desc("`_` and `_x7`"), "`_` and `_x7`");
        assert_eq!(norm_desc("  "), "-");
        // …and what it erases, in order, is what tells twins apart.
        assert_eq!(
            erase("(count `_14`) for new::<__trust_args@f3f39371aeb0> (count `_8`)").1,
            ["_14", "@f3f39371aeb0", "_8"]
        );
        // Normalising twice is normalising once: a record that kept the
        // normalised form compares alike.
        let once = norm_desc("(count `_14`) @f3f39371aeb0");
        assert_eq!(norm_desc(&once), once);
    }

    #[test]
    fn the_anchor_is_the_spans_text_by_characters() {
        let src: Vec<String> = [
            "pub fn unicode(a: u8) -> u8 {",
            "    let _s = \"éé—\"; if a < 10 { a + 1 } else { 0 }",
            "}",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        // Column 32 counts characters; its byte offset is 36.
        let one = Span {
            l0: 2,
            c0: 32,
            l1: 2,
            c1: 37,
        };
        assert_eq!(anchor_of(&src, one).unwrap(), "a + 1");
        let whole = Span {
            l0: 1,
            c0: 0,
            l1: 3,
            c1: 1,
        };
        assert_eq!(
            anchor_of(&src, whole).unwrap(),
            "pub fn unicode(a: u8) -> u8 { … }"
        );
        let long = vec!["x".repeat(150)];
        let a = anchor_of(
            &long,
            Span {
                l0: 1,
                c0: 0,
                l1: 1,
                c1: 150,
            },
        )
        .unwrap();
        assert!(a.starts_with(&"x".repeat(80)) && a.contains("…#"), "{a}");
        assert_eq!(a.chars().count(), 80 + 2 + 12);
        for bad in [
            Span {
                l0: 4,
                c0: 0,
                l1: 4,
                c1: 1,
            },
            Span {
                l0: 3,
                c0: 0,
                l1: 3,
                c1: 2,
            },
            Span {
                l0: 0,
                c0: 0,
                l1: 1,
                c1: 1,
            },
        ] {
            assert!(anchor_of(&src, bad).is_err(), "{bad}");
        }
    }

    /// A checkout with `src/lib.rs` (and a fork whose workspace is `vendor/w`).
    fn checkout() -> aterm_tempfile::TempDir {
        let d = tmpdir();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/lib.rs"),
            "pub fn twice(a: u16) -> u16 {\n    let x = if a < 10 { a + 1 } else { 0 };\n    x + x\n}\n",
        )
        .unwrap();
        fs::create_dir_all(d.path().join("vendor/w/crates/f/src")).unwrap();
        fs::write(d.path().join("vendor/w/crates/f/src/lib.rs"), "fn f() {}\n").unwrap();
        d
    }

    #[test]
    fn files_are_named_as_the_keys_compare_them() {
        let d = checkout();
        let repo = d.path();
        let s = Sources::new(repo, None).unwrap();
        assert_eq!(
            s.resolve("src/lib.rs").unwrap(),
            ("src/lib.rs".into(), true)
        );
        // The seal's remapped standard library, and a scratch stage2's.
        assert_eq!(
            s.resolve("alloc/src/macros.rs").unwrap(),
            ("@sysroot/alloc/src/macros.rs".into(), false)
        );
        assert_eq!(
            s.resolve("/Users//user000/trust/library/alloc/src/macros.rs")
                .unwrap(),
            ("@sysroot/alloc/src/macros.rs".into(), false)
        );
        assert_eq!(
            s.resolve("/rustc/0123abcd/library/core/src/num/mod.rs")
                .unwrap(),
            ("@sysroot/core/src/num/mod.rs".into(), false)
        );
        assert_eq!(
            s.resolve(
                "/h/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/log-0.4.32/src/lib.rs"
            )
            .unwrap(),
            ("@registry/log-0.4.32/src/lib.rs".into(), false)
        );
        let abs = format!("{}/src/lib.rs", repo.display());
        assert_eq!(s.resolve(&abs).unwrap(), ("src/lib.rs".into(), true));
        // A fork's paths, relative to its own workspace's root.
        let fork = Sources::new(repo, Some("vendor/w/crates/f/Cargo.toml")).unwrap();
        assert_eq!(
            fork.resolve("crates/f/src/lib.rs").unwrap(),
            ("vendor/w/crates/f/src/lib.rs".into(), true)
        );
        assert_eq!(
            fork.resolve("src/lib.rs").unwrap(),
            ("vendor/w/crates/f/src/lib.rs".into(), true)
        );
        // Of no known form: refused.
        for bad in [
            "/opt/elsewhere/x.rs",
            "src/missing.rs",
            "../src/lib.rs",
            "",
            "src//lib.rs",
        ] {
            assert!(s.resolve(bad).is_err(), "{bad}");
        }
    }

    /// A report.ndjson line for a function with `obligations` (kind, status,
    /// file, l0, c0, l1, c1, description).
    type Obl<'a> = (&'a str, &'a str, &'a str, u32, u32, u32, u32, &'a str);

    const UNIT: &str = r#"cargo-target(package_id=\"path+file:///x#d27probe@0.1.0\",package=\"d27probe\",kind=[\"lib\"],target=\"d27probe\")"#;

    fn function_line(path: &str, obls: &[Obl<'_>]) -> String {
        let proved = obls.iter().filter(|o| o.1 == "proved").count();
        let os: Vec<String> = obls
            .iter()
            .map(|(k, st, f, l0, c0, l1, c1, d)| {
                format!(
                    r#"{{"obligation_id":"vc:x:y:0","description":"{d}","kind":"{k}","location":{{"file":"{f}","line_start":{l0},"col_start":{c0},"line_end":{l1},"col_end":{c1}}},"outcome":{{"status":"{st}"}},"proof_evidence":{{"artifacts":[{{"deep":[[[1]]]}}]}},"time_ms":0}}"#
                )
            })
            .collect();
        format!(
            r#"{{"record_type":"function","crate_name":"{UNIT}","function":"{UNIT}::{path}","summary":{{"total_obligations":{},"proved":{proved},"verdict":"x"}},"obligations":[{}]}}"#,
            obls.len(),
            os.join(",")
        )
    }

    fn report(functions: &[String]) -> String {
        let total: usize = functions
            .iter()
            .map(|f| f.matches("\"obligation_id\"").count())
            .sum();
        let proved: usize = functions
            .iter()
            .map(|f| f.matches(r#""status":"proved""#).count())
            .sum();
        let mut out = format!(
            r#"{{"record_type":"header","schema":"{REPORT_SCHEMA}","crate_name":"{UNIT}","expected_functions":{}}}"#,
            functions.len()
        );
        out.push('\n');
        for f in functions {
            out.push_str(f);
            out.push('\n');
        }
        out.push_str(&format!(
            r#"{{"record_type":"footer","schema":"{REPORT_SCHEMA}","summary":{{"total_obligations":{total},"total_proved":{proved},"verdict":"x"}},"functions_emitted":{}}}"#,
            functions.len()
        ));
        out.push('\n');
        out
    }

    fn probe_report() -> String {
        report(&[
            function_line(
                "d27probe::twice",
                &[
                    (
                        "arithmetic_overflow_add",
                        "proved",
                        "src/lib.rs",
                        2,
                        24,
                        2,
                        29,
                        "arithmetic overflow (Add)",
                    ),
                    (
                        "arithmetic_overflow_add",
                        "unknown",
                        "src/lib.rs",
                        3,
                        4,
                        3,
                        9,
                        "arithmetic overflow (Add)",
                    ),
                ],
            ),
            function_line(
                "d27probe::zeros_a",
                &[(
                    "unbounded_allocation",
                    "proved",
                    "alloc/src/macros.rs",
                    47,
                    8,
                    47,
                    41,
                    "unbounded allocation in `vec::from_elem` (count `_7`)",
                )],
            ),
        ])
    }

    fn reduce_text(text: &str, repo: &Path) -> std::result::Result<Record, String> {
        let d = tmpdir();
        let p = d.path().join("report.ndjson");
        fs::write(&p, text).unwrap();
        reduce(&p, "d27probe", repo, None)
    }

    #[test]
    fn a_report_reduces_to_a_record_that_reads_back() {
        let d = checkout();
        let rec = reduce_text(&probe_report(), d.path()).unwrap();
        assert_eq!((rec.functions, rec.total(), rec.proved()), (2, 3, 2));
        let o = &rec.rows[0];
        assert_eq!(o.function, "d27probe::twice");
        assert_eq!(o.anchor, "a + 1");
        assert_eq!(o.site(), "src/lib.rs:2:24-2:29");
        assert_eq!(rec.rows[1].anchor, "x + x");
        let z = &rec.rows[2];
        assert_eq!(z.file, "@sysroot/alloc/src/macros.rs");
        assert_eq!(
            (z.anchor.as_str(), z.site().as_str()),
            ("-", "@sysroot/alloc/src/macros.rs")
        );
        // The record keeps the description as reported; the keys normalise it,
        // and what normalising erases tells twins apart.
        assert!(z.desc.ends_with("(count `_7`)"), "{}", z.desc);
        assert!(z.norm().ends_with("(count `_`)"), "{}", z.norm());
        assert_eq!(
            z.twin(),
            (
                Some(Span {
                    l0: 47,
                    c0: 8,
                    l1: 47,
                    c1: 41
                }),
                "_7".to_string()
            )
        );
        assert_eq!(rec.rows[0].twin(), (None, String::new()));
        assert_eq!(Record::parse(&rec.to_text()).unwrap(), rec);
        // An obligation the report gives no location is keyed by its function, kind
        // and description.
        let none = probe_report().replacen(
            r#""location":{"file":"src/lib.rs","line_start":3,"col_start":4,"line_end":3,"col_end":9},"#,
            "",
            1,
        );
        let rec = reduce_text(&none, d.path()).unwrap();
        let o = &rec.rows[1];
        assert_eq!(
            (o.file.as_str(), o.anchor.as_str(), o.site().as_str()),
            ("@none", "-", "@none")
        );
        assert_eq!(o.span.to_string(), "0:0-0:0");
        assert_eq!(Record::parse(&rec.to_text()).unwrap(), rec);
        assert!(
            Record::parse(
                &rec.to_text()
                    .replace("\t@none\t0\t0\t0\t0\t", "\t@none\t1\t0\t1\t0\t")
            )
            .is_err()
        );
    }

    #[test]
    fn every_report_it_cannot_vouch_for_is_refused_with_its_reason() {
        let d = checkout();
        let good = probe_report();
        let lines: Vec<&str> = good.lines().collect();
        let join = |v: &[&str]| v.join("\n") + "\n";
        let cases: Vec<(String, &str)> = vec![
            (String::new(), "the report is empty"),
            (join(&lines[..3]), "no footer: it was cut short"),
            (
                join(&[lines[0], "not json", lines[3]]),
                "is not a report record",
            ),
            (join(&[lines[1], lines[0]]), "before the header"),
            (
                good.replace(REPORT_SCHEMA, "trust.report.ndjson.v3"),
                "schema trust.report.ndjson.v3",
            ),
            (
                good.replace("package=\\\"d27probe\\\"", "package=\\\"other\\\""),
                "the report is of package other, not d27probe",
            ),
            (
                join(&[
                    lines[0],
                    lines[1],
                    r#"{"record_type":"transport","schema":"x"}"#,
                    lines[2],
                    lines[3],
                ]),
                "a `transport` record",
            ),
            (
                good.replace("::d27probe::twice", "d27probe::twice"),
                "is not of the form <unit>::<path>",
            ),
            (
                good.replace("\"arithmetic_overflow_add\"", "\"Arith Overflow\""),
                "the kind `Arith Overflow`",
            ),
            (
                good.replace("\"arithmetic_overflow_add\"", "\"transport:missing-json\""),
                "a `transport:missing-json` row",
            ),
            (
                good.replace("\"line_end\":2,", "\"line_end\":1,"),
                "ends before it starts",
            ),
            (
                good.replace("src/lib.rs", "/opt/x/lib.rs"),
                "an absolute path of no form",
            ),
            (
                good.replace("\"line_start\":3", "\"line_start\":30")
                    .replace("\"line_end\":3", "\"line_end\":30"),
                "past the file",
            ),
            (
                good.replace("\"total_proved\":2", "\"total_proved\":3"),
                "the footer says",
            ),
            (
                good.replace("\"functions_emitted\":2", "\"functions_emitted\":3"),
                "the footer says",
            ),
            (
                good.replace("\"proved\":1,", "\"proved\":2,"),
                "summary is not its",
            ),
            (
                good.replace("\"status\":\"unknown\"", "\"status\":\"Unknown!\""),
                "the status `Unknown!`",
            ),
            (
                good.replacen(r#""file":"src/lib.rs""#, r#""file":"@none""#, 1),
                "@none names no file in the checkout",
            ),
            (format!("{good}{}\n", lines[1]), "follows the footer"),
            (
                join(&[lines[0], lines[1], lines[1], lines[3]])
                    .replace("\"functions_emitted\":2", "\"functions_emitted\":3"),
                "d27probe::twice is reported a second time",
            ),
        ];
        for (text, said) in cases {
            let why = reduce_text(&text, d.path()).expect_err(said);
            assert!(why.contains(said), "{said}: {why}");
        }
    }

    #[test]
    fn a_record_that_is_not_whole_is_refused() {
        let d = checkout();
        let text = reduce_text(&probe_report(), d.path()).unwrap().to_text();
        assert!(Record::parse(&text).is_ok());
        let cut = text
            .lines()
            .filter(|l| !l.starts_with("end\t"))
            .collect::<Vec<_>>()
            .join("\n");
        for (bad, said) in [
            (cut, "cut short"),
            (
                text.replace("proved\t2\n", "proved\t3\n"),
                "it says 3 proved",
            ),
            (
                text.replace("total\t3\n", "total\t4\n"),
                "it says 4 obligations",
            ),
            (text.replacen("\tproved\t", "\tpro ved\t", 1), "the status"),
            (text.replace("@sysroot/alloc", "/abs/alloc"), "the file"),
            (text.replace(RECORD_MAGIC, "# something else"), "first line"),
            (format!("{text}o\tx\n"), "follows its `end` line"),
        ] {
            let why = Record::parse(&bad).expect_err(said);
            assert!(why.contains(said), "{said}: {why}");
        }
    }

    /// A record of rows `(function, kind, file:l0, status, anchor, desc)`; an
    /// out-of-tree file is given with `@`.
    fn rec(rows: &[(&str, &str, &str, u32, &str, &str, &str)]) -> Record {
        Record {
            package: "lib".into(),
            unit: "cargo-target(package=\"lib\")".into(),
            functions: 1,
            rows: rows
                .iter()
                .map(|(f, k, file, l, st, a, d)| Obligation {
                    function: (*f).into(),
                    kind: (*k).into(),
                    file: (*file).into(),
                    span: Span {
                        l0: *l,
                        c0: 4,
                        l1: *l,
                        c1: 9,
                    },
                    status: (*st).into(),
                    anchor: (*a).into(),
                    desc: (*d).into(),
                })
                .collect(),
        }
    }

    const ADD: &str = "arithmetic_overflow_add";

    fn no_ledger(_: &Retraction) -> Contains {
        Contains::Unknown("no ledger".into())
    }

    #[test]
    fn a_gain_never_offsets_a_loss() {
        // The seal proves f's line 3; the candidate loses it and gains two others.
        let base = rec(&[
            ("f", ADD, "src/lib.rs", 3, "proved", "a + 1", "add"),
            ("f", ADD, "src/lib.rs", 4, "failed", "a + 2", "add"),
            ("g", ADD, "src/lib.rs", 9, "unknown", "b + 1", "add"),
        ]);
        let cand = rec(&[
            ("f", ADD, "src/lib.rs", 3, "failed", "a + 1", "add"),
            ("f", ADD, "src/lib.rs", 4, "proved", "a + 2", "add"),
            ("g", ADD, "src/lib.rs", 9, "proved", "b + 1", "add"),
        ]);
        let c = compare_lib(&base, &cand, &[], &mut here, &mut no_ledger);
        assert_eq!(c.gained, 2);
        assert_eq!(c.lost.len(), 1);
        assert_eq!(c.lost[0].key.anchor, "a + 1");
        assert_eq!(
            (c.lost[0].cand.as_str(), c.lost[0].missing),
            ("failed", false)
        );
        assert!(c.lost[0].note.is_empty(), "{c:?}");
        assert_eq!(c.functions_lost(), 1);
    }

    #[test]
    fn a_cell_is_held_to_its_worst_case() {
        // (n, m, p, q) -> lost: one a side is the ordinary case…
        for (n, m, p, q, lost) in [
            (1, 1, 1, 1, 0),
            (1, 1, 1, 0, 1),
            (1, 1, 0, 1, 0),
            (1, 0, 1, 0, 1),
            (0, 1, 0, 1, 0),
            // …twins lose what the candidate's unproved (or absent) could hide.
            (2, 2, 1, 1, 1),
            (2, 2, 2, 1, 1),
            (2, 2, 1, 2, 0),
            (2, 2, 0, 1, 0),
            (1, 2, 1, 1, 1),
            (2, 1, 2, 1, 1),
            (2, 1, 1, 1, 1),
            (3, 2, 1, 2, 1),
            (2, 3, 2, 3, 0),
            (3, 3, 3, 0, 3),
        ] {
            assert_eq!(cell_lost(n, m, p, q), lost, "{n} {m} {p} {q}");
        }
    }

    /// Two obligations of `c` at one in-tree site (the probe's closure `|j| v[j] +
    /// v[j]`: two `slice bounds check`s at one span), statuses `a` and `b`.
    fn site(a: &str, b: &str) -> Record {
        let rows: Vec<_> = [a, b]
            .into_iter()
            .filter(|st| !st.is_empty())
            .map(|st| {
                (
                    "c",
                    "slice_bounds_check",
                    "src/lib.rs",
                    34,
                    st,
                    "v[i]",
                    "slice bounds check",
                )
            })
            .collect();
        rec(&rows)
    }

    #[test]
    fn twins_nothing_tells_apart_lose_at_their_worst_and_a_missing_key_is_lost() {
        let lost = |b: &Record, c: &Record| compare_lib(b, c, &[], &mut here, &mut no_ledger);
        // [failed, failed] under the seal, [failed, proved] under the candidate: a
        // gain, no loss.
        let c = lost(&site("failed", "failed"), &site("failed", "proved"));
        assert!(c.lost.is_empty() && c.gained == 1, "{c:?}");
        let c = lost(&site("proved", "proved"), &site("failed", "proved"));
        assert_eq!((c.lost.len(), c.lost[0].key.nth), (1, 1), "{c:?}");
        // The seal proves one, the candidate the other: a count of statuses sees no
        // loss, but which one the candidate proves cannot be told, so one is lost.
        let c = lost(&site("proved", "failed"), &site("failed", "proved"));
        assert_eq!(c.lost.len(), 1, "{c:?}");
        assert!(
            c.lost[0].note.starts_with("twins: 2 under the published seal and 2 under the candidate at this key, which nothing reported tells apart; the candidate proves 1 of its 2, so 1 of the 1 the seal proved may be lost"),
            "{c:?}"
        );
        assert_eq!(c.lost[0].cand, "failed, proved");
        // …which is the price of an unchanged candidate there too.
        assert_eq!(
            lost(&site("proved", "failed"), &site("proved", "failed"))
                .lost
                .len(),
            1
        );
        // Every twin proved: nothing lost.
        let c = lost(&site("proved", "failed"), &site("proved", "proved"));
        assert!(c.lost.is_empty() && c.gained == 1, "{c:?}");
        // A duplicate proved row cannot hide a failed one, nor a twin fewer.
        assert_eq!(
            lost(&site("proved", ""), &site("failed", "proved"))
                .lost
                .len(),
            1
        );
        assert_eq!(
            lost(&site("proved", "proved"), &site("proved", ""))
                .lost
                .len(),
            1
        );
        // The seal proves an obligation the candidate does not report at all.
        let base = rec(&[("f", ADD, "src/lib.rs", 3, "proved", "a + 1", "add")]);
        let cand = rec(&[("f2", ADD, "src/lib.rs", 3, "proved", "a + 1", "add")]);
        let c = lost(&base, &cand);
        assert_eq!((c.lost.len(), c.missing(), c.gained), (1, 1, 1));
        assert_eq!(c.lost[0].cand, "MISSING");
        // An unproved seal obligation the candidate drops is no loss.
        let base = rec(&[("f", ADD, "src/lib.rs", 3, "unknown", "a + 1", "add")]);
        assert!(lost(&base, &rec(&[])).lost.is_empty());
        // Out of the tree the position is not compared: the standard library differs.
        let at = |l: u32| {
            rec(&[(
                "f",
                "unbounded_allocation",
                "@sysroot/alloc/src/macros.rs",
                l,
                "proved",
                "-",
                "vec",
            )])
        };
        assert!(lost(&at(47), &at(52)).lost.is_empty());
    }

    #[test]
    fn twins_are_told_apart_by_what_both_runs_report_of_them() {
        // The seal reports `(vec![0u8; x], vec![0u8; y])`'s two allocations at
        // `alloc/src/macros.rs:47:8-47:41`, `_8`'s proved, `_10`'s failed.
        let zeros = |s8: &str, s10: &str, at: u32| {
            let one = |st: &str, local: &str| Obligation {
                function: "zeros".into(),
                kind: "unbounded_allocation".into(),
                file: "@sysroot/alloc/src/macros.rs".into(),
                span: Span {
                    l0: at,
                    c0: 8,
                    l1: at,
                    c1: 41,
                },
                status: st.into(),
                anchor: "-".into(),
                desc: format!("unbounded allocation in `vec::from_elem` (count `{local}`)"),
            };
            Record {
                rows: vec![one(s8, "_8"), one(s10, "_10")],
                ..rec(&[])
            }
        };
        let seal = zeros("proved", "failed", 47);
        assert_eq!(seal.rows[0].exact(), seal.rows[1].exact());
        let c = compare_lib(
            &seal,
            &zeros("failed", "proved", 47),
            &[],
            &mut here,
            &mut no_ledger,
        );
        assert_eq!(c.lost.len(), 1, "{c:?}");
        assert!(c.lost[0].note.is_empty(), "told apart: {c:?}");
        assert_eq!((c.lost[0].cand.as_str(), c.gained), ("failed", 1));
        let c = compare_lib(
            &seal,
            &zeros("proved", "failed", 47),
            &[],
            &mut here,
            &mut no_ledger,
        );
        assert!(c.lost.is_empty() && c.gained == 0, "{c:?}");
        // A standard library that moved: positions differ, so the key is one cell.
        let c = compare_lib(
            &seal,
            &zeros("proved", "failed", 52),
            &[],
            &mut here,
            &mut no_ledger,
        );
        assert_eq!(c.lost.len(), 1, "{c:?}");
        // Out-of-tree twins told apart by position alone (an inlined growth's two
        // checks at raw_vec.rs 100 and 200): the one at 200 is the one lost.
        let grow = |x: &str, y: &str, shift: u32| {
            rec(&[
                (
                    "grow",
                    "arithmetic_overflow_mul",
                    "@sysroot/alloc/src/raw_vec.rs",
                    100 + shift,
                    x,
                    "-",
                    "arithmetic overflow (Mul)",
                ),
                (
                    "grow",
                    "arithmetic_overflow_mul",
                    "@sysroot/alloc/src/raw_vec.rs",
                    200 + shift,
                    y,
                    "-",
                    "arithmetic overflow (Mul)",
                ),
            ])
        };
        let c = compare_lib(
            &grow("proved", "proved", 0),
            &grow("proved", "failed", 0),
            &[],
            &mut here,
            &mut no_ledger,
        );
        assert_eq!((c.lost.len(), c.lost[0].key.nth), (1, 2), "{c:?}");
        assert!(c.lost[0].note.is_empty(), "{c:?}");
    }

    #[test]
    fn nth_orders_a_functions_twins_by_position() {
        let base = rec(&[
            (
                "e",
                ADD,
                "src/g.rs",
                245,
                "proved",
                "cursor_col + count",
                "add",
            ),
            (
                "e",
                ADD,
                "src/g.rs",
                229,
                "proved",
                "cursor_col + count",
                "add",
            ),
            ("e", ADD, "src/g.rs", 100, "proved", "other", "add"),
        ]);
        let keys = ledger_keys(&base);
        assert_eq!(
            keys.iter().map(|k| (k.nth, k.of)).collect::<Vec<_>>(),
            [(2, 2), (1, 2), (1, 1)]
        );
        assert_eq!(
            keys[0].columns(),
            "e\tarithmetic_overflow_add\tsrc/g.rs\tcursor_col + count\t2/2\tadd"
        );
        // …and a shift of every line keeps every key.
        let mut shifted = base.clone();
        for o in &mut shifted.rows {
            o.span.l0 += 6;
            o.span.l1 += 6;
        }
        assert_eq!(ledger_keys(&shifted), keys);
    }

    /// The aterm commit the runs below measured, and the rows' audits too.
    const ATERM: &str = "36b2c3df36eac4971b8189da9967ab7cc442bbf2";
    /// The published seal's trust commit the rows were audited against.
    const SEAL: &str = "321aaeda75478038420d97830f14724c33c8dc39";

    /// A retraction of `key`, audited at [`ATERM`] where the seal reported it at
    /// line `l` (as [`rec`] places it).
    fn retraction(line: usize, key: Key, l: u32, commit: &str) -> Retraction {
        Retraction {
            line,
            library: "lib".into(),
            key,
            at: Span {
                l0: l,
                c0: 4,
                l1: l,
                c1: 9,
            },
            aterm: ATERM.into(),
            seal: SEAL.into(),
            commit: commit.into(),
            test_path: "crates/trust-vcgen/tests/false_proofs.rs".into(),
            test_fn: "guard_by_reaching_def".into(),
            reviewed: "reviewer 2026-10-07 class A".into(),
        }
    }

    const FIX: &str = "3a5a9ea10d1075bd53300dd41dbc0b9738e1e852";

    fn yes(_: &Retraction) -> Contains {
        Contains::Yes
    }

    /// Every row audited in the tree measured.
    fn here(_: &Retraction) -> Audited {
        Audited::Same
    }

    #[test]
    fn a_retraction_excuses_its_own_obligation_only_with_the_fix_in_the_candidate_and_it_unproved()
    {
        let base = rec(&[
            ("f", ADD, "src/lib.rs", 3, "proved", "a + 1", "add"),
            ("f", ADD, "src/lib.rs", 5, "proved", "a + 1", "add"),
        ]);
        let lost = rec(&[
            ("f", ADD, "src/lib.rs", 3, "failed", "a + 1", "add"),
            ("f", ADD, "src/lib.rs", 5, "proved", "a + 1", "add"),
        ]);
        let first = ledger_keys(&base)[0].clone();
        assert_eq!((first.nth, first.of), (1, 2));
        let r = retraction(4, first.clone(), 3, FIX);
        let c = compare_lib(&base, &lost, &[&r], &mut here, &mut yes);
        assert!(c.lost.is_empty() && c.stale.is_empty(), "{c:?}");
        assert_eq!((c.excused.len(), c.excused[0].line), (1, 4));
        // Without the fix in the candidate: the loss refuses, naming the row.
        let c = compare_lib(&base, &lost, &[&r], &mut here, &mut |_: &Retraction| {
            Contains::No("the candidate does not contain trust 3a5a9ea10d10".into())
        });
        assert_eq!(c.lost.len(), 1);
        assert!(c.lost[0].note.contains("retraction line 4 does not apply: the candidate does not contain trust 3a5a9ea10d10"), "{c:?}");
        // …and when it cannot be told.
        let c = compare_lib(&base, &lost, &[&r], &mut here, &mut |_: &Retraction| {
            Contains::Unknown("no trust commit".into())
        });
        assert_eq!((c.lost.len(), c.excused.len()), (1, 0));
        // A candidate that still proves it: the retraction is STALE.
        let c = compare_lib(&base, &base, &[&r], &mut here, &mut yes);
        assert!(c.lost.is_empty() && c.excused.is_empty());
        assert_eq!(c.stale.len(), 1);
        assert!(c.stale[0].1.contains("still proves"), "{:?}", c.stale);
        // A row excuses its own obligation, never another: the candidate still
        // proves row 1 (STALE) and loses row 2, which no row names.
        let other = rec(&[
            ("f", ADD, "src/lib.rs", 3, "proved", "a + 1", "add"),
            ("f", ADD, "src/lib.rs", 5, "failed", "a + 1", "add"),
        ]);
        let c = compare_lib(&base, &other, &[&r], &mut here, &mut yes);
        assert_eq!((c.lost.len(), c.excused.len(), c.stale.len()), (1, 0, 1));
        assert_eq!(c.lost[0].key.nth, 2);
        // Two losses, one row: one loss left.
        let both = rec(&[
            ("f", ADD, "src/lib.rs", 3, "failed", "a + 1", "add"),
            ("f", ADD, "src/lib.rs", 5, "failed", "a + 1", "add"),
        ]);
        let c = compare_lib(&base, &both, &[&r], &mut here, &mut yes);
        assert_eq!((c.lost.len(), c.excused.len()), (1, 1));
        assert_eq!(c.lost[0].key.nth, 2);
        // A row naming what the seal did not prove, or nothing at all: UNUSED.
        let mut nothing = first.clone();
        nothing.nth = 3;
        let r2 = retraction(9, nothing, 3, FIX);
        let c = compare_lib(&base, &lost, &[&r2], &mut here, &mut yes);
        assert_eq!(c.notes[0].1, "UNUSED");
        assert_eq!(c.lost.len(), 1);
        // A MISSING obligation no row excuses.
        let gone = rec(&[("f", ADD, "src/lib.rs", 5, "proved", "a + 1", "add")]);
        let c = compare_lib(&base, &gone, &[&r], &mut here, &mut yes);
        assert_eq!((c.lost.len(), c.missing()), (1, 1));
        assert!(c.lost[0].note.contains("MISSING"), "{c:?}");
        // A row audited when its group was another size (`1/1`, before row 5
        // was added beside it) names nothing for certain: not applied.
        let audited = retraction(
            7,
            Key {
                of: 1,
                ..first.clone()
            },
            3,
            FIX,
        );
        let c = compare_lib(&base, &lost, &[&audited], &mut here, &mut yes);
        assert_eq!((c.lost.len(), c.excused.len()), (1, 0), "{c:?}");
        assert!(
            c.lost[0].note.contains("retraction line 7 does not apply: the published seal reports 2 obligations sharing its fields and the row was audited at 1"),
            "{c:?}"
        );
    }

    #[test]
    fn a_retraction_among_twins_applies_only_where_its_own_status_is_told() {
        let r = |key: Key| retraction(4, key, 34, FIX);
        let base = site("proved", "proved");
        let key = ledger_keys(&base)[0].clone();
        assert_eq!((key.nth, key.of), (1, 2));
        // The candidate proves one of the two: which cannot be told.
        let c = compare_lib(
            &base,
            &site("failed", "proved"),
            &[&r(key.clone())],
            &mut here,
            &mut yes,
        );
        assert_eq!((c.lost.len(), c.excused.len(), c.stale.len()), (1, 0, 0));
        assert!(
            c.lost
                .iter()
                .any(|l| l.note.contains("whether it proves this one cannot be told")),
            "{c:?}"
        );
        // It proves neither: the row's own is not proved, and is excused.
        let c = compare_lib(
            &base,
            &site("failed", "failed"),
            &[&r(key.clone())],
            &mut here,
            &mut yes,
        );
        assert_eq!((c.lost.len(), c.excused.len()), (1, 1), "{c:?}");
        assert_eq!(c.lost[0].key.nth, 2);
        // It proves both: the row's own is proved — STALE.
        let c = compare_lib(
            &base,
            &site("proved", "proved"),
            &[&r(key)],
            &mut here,
            &mut yes,
        );
        assert_eq!((c.lost.len(), c.excused.len(), c.stale.len()), (0, 0, 1));
    }

    /// A row audited for X, the first of two `a + 1` in `f` (X at line 3, Z at line 7:
    /// `1/2`), after an edit that keeps the group at two: its rank then names an
    /// obligation nobody audited.
    #[test]
    fn a_retraction_applies_only_in_the_tree_and_at_the_site_it_was_audited_at() {
        let add = |w: (u32, &str), z: (u32, &str)| {
            rec(&[
                ("f", ADD, "src/lib.rs", w.0, w.1, "a + 1", "add"),
                ("f", ADD, "src/lib.rs", z.0, z.1, "a + 1", "add"),
            ])
        };
        // As audited: X at 3, Z at 7, its key `1/2`, its site line 3.
        let audited = add((3, "proved"), (7, "proved"));
        let key = ledger_keys(&audited)[0].clone();
        assert_eq!((key.nth, key.of), (1, 2));
        let r = retraction(4, key, 3, FIX);
        fn changed(_: &Retraction) -> Audited {
            Audited::Changed("src/lib.rs changed since the audit".into())
        }
        // Control, in the tree it was audited in: the candidate loses X, and the row
        // excuses it.
        let c = compare_lib(
            &audited,
            &add((3, "failed"), (7, "proved")),
            &[&r],
            &mut here,
            &mut yes,
        );
        assert_eq!((c.lost.len(), c.excused.len()), (0, 1), "{c:?}");
        // Replaced: X deleted and W added at line 2, the group still two. The candidate
        // loses W, which nobody audited: W is not at the audited site, so the row does
        // not apply even where the tree reads as the audited one…
        let replaced = add((2, "proved"), (7, "proved"));
        let lose_w = add((2, "failed"), (7, "proved"));
        let c = compare_lib(&replaced, &lose_w, &[&r], &mut here, &mut yes);
        assert_eq!((c.lost.len(), c.excused.len()), (1, 0), "{c:?}");
        assert!(
            c.lost[0].note.contains("retraction line 4 does not apply: the obligation its key names is at src/lib.rs:2:4-2:9 under the published seal, and the row was audited at src/lib.rs:3:4-3:9"),
            "{c:?}"
        );
        // …and, the tree changed, not at all.
        let c = compare_lib(&replaced, &lose_w, &[&r], &mut changed, &mut yes);
        assert_eq!((c.lost.len(), c.excused.len()), (1, 0), "{c:?}");
        assert!(
            c.lost[0]
                .note
                .contains("retraction line 4 does not apply: src/lib.rs changed since the audit"),
            "{c:?}"
        );
        // Reordered: Z moved above X (Z at 3, X at 7). The row's rank and site now name
        // Z, which the candidate loses while it still proves X: only the tree can tell.
        let reordered = add((3, "proved"), (7, "proved"));
        let lose_z = add((3, "failed"), (7, "proved"));
        let c = compare_lib(&reordered, &lose_z, &[&r], &mut changed, &mut yes);
        assert_eq!(
            (c.lost.len(), c.excused.len(), c.stale.len()),
            (1, 0, 0),
            "{c:?}"
        );
        assert_eq!(c.lost[0].site, "src/lib.rs:3:4-3:9");
        assert_eq!(
            c.notes,
            [(
                4,
                "NOT-APPLIED",
                "src/lib.rs changed since the audit".to_string()
            )]
        );
        // …and when whether the tree is the audited one cannot be told.
        let c = compare_lib(
            &reordered,
            &lose_z,
            &[&r],
            &mut |_: &Retraction| Audited::Unknown("no aterm repository".into()),
            &mut yes,
        );
        assert_eq!((c.lost.len(), c.excused.len()), (1, 0), "{c:?}");
        // Deleted and added again, 1/1: the one `a + 1` replaced by another at the same
        // place. Rank, size and site all agree; the tree does not.
        let alone = rec(&[("f", ADD, "src/lib.rs", 3, "proved", "a + 1", "add")]);
        let one = retraction(5, ledger_keys(&alone)[0].clone(), 3, FIX);
        let lost = rec(&[("f", ADD, "src/lib.rs", 3, "failed", "a + 1", "add")]);
        let c = compare_lib(&alone, &lost, &[&one], &mut changed, &mut yes);
        assert_eq!((c.lost.len(), c.excused.len()), (1, 0), "{c:?}");
        // NOT-APPLIED, never UNUSED (which would say to prune it): a row naming nothing in a
        // changed tree, and one naming an unproved obligation at another site in its own tree.
        let mut none = r.key.clone();
        none.nth = 2;
        none.of = 2;
        let c = compare_lib(
            &alone,
            &lost,
            &[&retraction(6, none, 3, FIX)],
            &mut changed,
            &mut yes,
        );
        assert_eq!(c.notes[0].1, "NOT-APPLIED", "{c:?}");
        let unproved = add((2, "failed"), (7, "proved"));
        let c = compare_lib(&unproved, &unproved, &[&r], &mut here, &mut yes);
        assert_eq!(c.notes[0].1, "NOT-APPLIED", "{c:?}");
        // The changed tree hides no staleness it could not see: a row that
        // applies nowhere is not STALE where the candidate proves what it names.
        let c = compare_lib(
            &audited,
            &add((3, "proved"), (7, "failed")),
            &[&r],
            &mut changed,
            &mut yes,
        );
        assert_eq!(
            (c.lost.len(), c.excused.len(), c.stale.len()),
            (1, 0, 0),
            "{c:?}"
        );
        assert_eq!(c.lost[0].key.nth, 2);
    }

    /// `git -C dir <args>` for a fixture, its output.
    fn fixture_git(dir: &Path, args: &[&str]) -> String {
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The aterm tree a row was audited in, against the tree measured: their
    /// trees compared entry by entry, but for the ledger itself.
    #[test]
    fn the_tree_audited_is_compared_entry_by_entry_but_for_the_ledger() {
        let d = tmpdir();
        let root = d.path().join("aterm");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(LEDGER_PATH).parent().unwrap()).unwrap();
        let g = |args: &[&str]| fixture_git(&root, args);
        let commit = |path: &str, text: &str, msg: &str| {
            fs::write(root.join(path), text).unwrap();
            g(&["add", "-A"]);
            g(&["commit", "-q", "-m", msg]);
            g(&["rev-parse", "HEAD"])
        };
        g(&["init", "-q", "-b", "main"]);
        fs::write(root.join(LEDGER_PATH), format!("{LEDGER_MAGIC}\n")).unwrap();
        let a = commit(
            "src/lib.rs",
            "pub fn f(a: u8) -> u8 {\n    a + 1\n}\n",
            "audited",
        );
        let ledgered = commit(
            LEDGER_PATH,
            &format!("{LEDGER_MAGIC}\n# a row\n"),
            "the ledger",
        );
        let edited = commit(
            "src/lib.rs",
            "pub fn f(b: u8) -> u8 {\n    b + 1\n}\n",
            "an edit",
        );
        let runner = crate::ledger::testing::Git::new(root.join(LEDGER_PATH).parent().unwrap());
        let repo = AtermRepo {
            git: &runner,
            shown: "the aterm repository".into(),
        };
        assert_eq!(repo.audited_tree(&a, &a), Ok(Audited::Same));
        assert_eq!(repo.audited_tree(&a, &ledgered), Ok(Audited::Same));
        match repo.audited_tree(&a, &edited) {
            Ok(Audited::Changed(why)) => assert!(
                why.contains("whose trees differ in 1 path(s) besides the ledger (src/lib.rs)"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
        // A commit the repository lacks: it cannot be told.
        let gone = "0123456789012345678901234567890123456789";
        assert!(
            matches!(repo.audited_tree(gone, &a), Ok(Audited::Unknown(why)) if why.contains("is not a commit of the aterm repository")),
            "{:?}",
            repo.audited_tree(gone, &a)
        );
        // A replace ref that makes the edit read as the audited commit is not
        // read.
        g(&["replace", &edited, &a]);
        assert_eq!(
            g(&["rev-parse", &format!("{edited}^{{tree}}")]),
            g(&["rev-parse", &format!("{a}^{{tree}}")])
        );
        assert!(matches!(
            repo.audited_tree(&a, &edited),
            Ok(Audited::Changed(_))
        ));
        g(&["replace", "-d", &edited]);
        // A submodule bump that `.gitmodules` (`ignore = all`) and the repository's
        // config (`diff.ignoreSubmodules`) both say to ignore, so that `git diff --quiet`
        // calls the trees the same, is a change.
        let gitlink = |sha: &str, msg: &str| {
            g(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},vendor/sub"),
            ]);
            g(&["commit", "-q", "-m", msg]);
            g(&["rev-parse", "HEAD"])
        };
        fs::write(
            root.join(".gitmodules"),
            "[submodule \"sub\"]\n\tpath = vendor/sub\n\turl = ./sub\n\tignore = all\n",
        )
        .unwrap();
        g(&["add", ".gitmodules"]);
        let s1 = gitlink("1111111111111111111111111111111111111111", "a submodule");
        let s2 = gitlink("2222222222222222222222222222222222222222", "its bump");
        g(&["config", "diff.ignoreSubmodules", "all"]);
        let quiet = crate::ledger::testing::git()
            .arg("-C")
            .arg(&root)
            .args(["diff", "--quiet", &s1, &s2])
            .status()
            .unwrap();
        assert!(
            quiet.success(),
            "git diff --quiet saw the bump: the fixture does not hide it"
        );
        match repo.audited_tree(&s1, &s2) {
            Ok(Audited::Changed(why)) => assert!(why.contains("(vendor/sub)"), "{why}"),
            other => panic!("a submodule bump read as {other:?}"),
        }
        // No repository at all: it cannot be told.
        let bare = tmpdir();
        let nowhere = crate::ledger::testing::Git::new(bare.path());
        let none = AtermRepo {
            git: &nowhere,
            shown: "a directory".into(),
        };
        assert!(
            matches!(none.audited_tree(&a, &edited), Ok(Audited::Unknown(why)) if why.starts_with("a directory is no git repository")),
            "{:?}",
            none.audited_tree(&a, &edited)
        );
    }

    #[test]
    fn the_ledger_is_held_to_its_form() {
        let libs: BTreeSet<String> = ["aterm-grid".to_string()].into();
        let site = "crates/aterm-grid/src/grid/line_ops.rs:245:15-245:33";
        let row = format!(
            "aterm-grid\tGrid::erase_chars\tarithmetic_overflow_add\tcrates/aterm-grid/src/grid/line_ops.rs\tcursor_col + count\t2/2\tarithmetic overflow (Add)\t{site}\t{ATERM}\t{SEAL}\t{FIX}\tcrates/trust-vcgen/tests/false_proofs.rs::guard_by_reaching_def\treviewer 2026-10-07 class A"
        );
        let ok = parse_ledger(
            &format!("{LEDGER_MAGIC}\n# a comment\n\n{row}\n"),
            "l.tsv",
            &libs,
        );
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
        assert_eq!(ok.rows[0].line, 4);
        assert_eq!((ok.rows[0].key.nth, ok.rows[0].key.of), (2, 2));
        assert_eq!(ok.rows[0].test_fn, "guard_by_reaching_def");
        assert_eq!(
            (
                ok.rows[0].site(),
                ok.rows[0].aterm.as_str(),
                ok.rows[0].seal.as_str()
            ),
            (site.to_string(), ATERM, SEAL)
        );
        // A site out of the tree, and one with no location.
        for (file, at) in [
            (
                "@sysroot/alloc/src/raw_vec.rs",
                "@sysroot/alloc/src/raw_vec.rs:100:8-100:30",
            ),
            ("@none", "@none:0:0-0:0"),
        ] {
            let l = parse_ledger(
                &format!(
                    "{LEDGER_MAGIC}\n{}",
                    row.replacen("crates/aterm-grid/src/grid/line_ops.rs", file, 1)
                        .replacen(site, at, 1)
                ),
                "l.tsv",
                &libs,
            );
            assert!(l.errors.is_empty(), "{at}: {:?}", l.errors);
            assert_eq!(l.rows[0].site(), at);
        }
        let empty = parse_ledger(&format!("{LEDGER_MAGIC}\n"), "l.tsv", &libs);
        assert!(empty.errors.is_empty() && empty.rows.is_empty());
        let edit = |from: &str, to: &str| row.replacen(from, to, 1);
        for (text, said) in [
            (row.clone(), "its first line is not"),
            (
                format!(
                    "# aterm trust-gate retractions v1\n{}",
                    edit("\t2/2\t", "\t2\t")
                ),
                "its first line is not",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\t2/2\t", "\t2\t")),
                "nth `2` is not <place>/<of>",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\t2/2\t", "\t0/2\t")),
                "nth `0/2`",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\t2/2\t", "\t3/2\t")),
                "nth `3/2`",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\t2/2\t", "\t02/2\t")),
                "nth `02/2`",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\t2/2\t", "\t2/*\t")),
                "nth `2/*`",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\t2/2\t", "\tsecond\t")),
                "nth `second`",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit(FIX, "3a5a9ea10d")),
                "not 40 lowercase hex",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("aterm-grid\t", "aterm-nope\t")),
                "in neither run's plan",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("::guard_by_reaching_def", "")),
                "is not <path in trust>::<test fn>",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit("arithmetic_overflow_add", "Overflow")
                ),
                "the kind `Overflow`",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit("crates/aterm-grid/src/grid/line_ops.rs", "/abs/line_ops.rs")
                ),
                "the file",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit("\tcursor_col + count\t", "\t\t")),
                "field 5 is empty",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit("\tcursor_col + count\t", "\t cursor_col\t")
                ),
                "padded",
            ),
            (format!("{LEDGER_MAGIC}\n{row}\textra"), "14 fields"),
            // A v2 ledger, its row of ten fields: refused whole, with its remedy.
            (
                format!(
                    "# aterm trust-gate retractions v2\n{}",
                    edit(&format!("\t{site}\t{ATERM}\t{SEAL}\t"), "\t")
                ),
                "a v2 row names its obligation only by its place among look-alikes",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(&format!("\t{site}\t{ATERM}\t{SEAL}\t"), "\t")
                ),
                "10 fields, and a retraction has exactly 13",
            ),
            // The site: its own file's span, nothing else.
            (
                format!("{LEDGER_MAGIC}\n{}", edit(site, "*")),
                "the site `*` is not",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(site, "crates/aterm-grid/src/grid/line_ops.rs")
                ),
                "the site `crates/aterm-grid/src/grid/line_ops.rs` is not",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(site, "crates/aterm-grid/src/grid/other.rs:245:15-245:33")
                ),
                "is not <its file>",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(site, "crates/aterm-grid/src/grid/line_ops.rs:245:15")
                ),
                "is not <its file>",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(site, "crates/aterm-grid/src/grid/line_ops.rs:0:15-0:33")
                ),
                "is not <its file>",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(site, "crates/aterm-grid/src/grid/line_ops.rs:245:33-245:15")
                ),
                "is not <its file>",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(
                        site,
                        "crates/aterm-grid/src/grid/line_ops.rs:0245:15-245:33"
                    )
                ),
                "is not <its file>",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(site, "crates/aterm-grid/src/grid/line_ops.rs:245:15-245:*")
                ),
                "is not <its file>",
            ),
            // The aterm commit: 40 lowercase hex.
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(&format!("\t{ATERM}\t"), "\t36b2c3df3\t")
                ),
                "the aterm commit `36b2c3df3` is not 40 lowercase hex",
            ),
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(
                        &format!("\t{ATERM}\t"),
                        &format!("\t{}\t", ATERM.to_uppercase())
                    )
                ),
                "is not 40 lowercase hex",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit(&format!("\t{ATERM}\t"), "\t*\t")),
                "the aterm commit `*`",
            ),
            // The seal's trust commit: 40 lowercase hex.
            (
                format!(
                    "{LEDGER_MAGIC}\n{}",
                    edit(&format!("\t{SEAL}\t"), "\t321aaeda7\t")
                ),
                "the seal's trust commit `321aaeda7` is not 40 lowercase hex",
            ),
            (
                format!("{LEDGER_MAGIC}\n{}", edit(&format!("\t{SEAL}\t"), "\t*\t")),
                "the seal's trust commit `*`",
            ),
            (
                format!("{LEDGER_MAGIC}\n{row}\n{row}"),
                "the same obligation",
            ),
            // Two rows naming one obligation, whatever group size each says.
            (
                format!("{LEDGER_MAGIC}\n{row}\n{}", edit("\t2/2\t", "\t2/3\t")),
                "the same obligation",
            ),
        ] {
            let l = parse_ledger(&text, "l.tsv", &libs);
            assert!(
                l.errors.iter().any(|e| e.contains(said)),
                "{said}: {:?}",
                l.errors
            );
        }
    }

    #[test]
    fn a_drivers_version_names_its_commit() {
        assert_eq!(
            driver_hashes("targo 1.99.0-dev (d36c551fa 2026-10-01) (targo 0.10.0+gd36c551fa)"),
            ["d36c551fa", "d36c551fa"]
        );
        assert_eq!(
            driver_hashes("targo 1.99.0-dev (321aaeda7 2026-09-17) (targo 0.1.0)"),
            ["321aaeda7"]
        );
        assert!(driver_hashes("targo 1.99.0-dev (stub)").is_empty());
    }
}
