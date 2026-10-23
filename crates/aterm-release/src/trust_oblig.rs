// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-obligation Trust verification records ([`Record`], [`REPORT_SCHEMA`], [`write_record`],
//! [`reduce`], [`Obligation::file`], [`anchor_of`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;
use serde::Deserialize;

use crate::trust_facts::{hex, one_line};

/// The record's first line.
pub const RECORD_MAGIC: &str = "# aterm trust obligations 1";
/// The one report schema [`reduce`] reads.
pub const REPORT_SCHEMA: &str = "trust.report.ndjson.v2";
/// The file of an obligation the report gives no location.
pub const NO_LOCATION: &str = "@none";
/// What the record of `<spec>.log` is called, beside it.
pub const RECORD_SUFFIX: &str = ".oblig.tsv";

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
}

/// One check's obligations, as the lane recorded them beside its log — tab-separated lines:.
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
    /// Repo-relative directories a relative path may be relative to.
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

    /// The normalised name of a file a report printed, and whether it is in the checkout.
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
fn chars(s: &str, a: u32, b: u32) -> Option<String> {
    let n = s.chars().count();
    let (a, b) = (a as usize, b as usize);
    (a <= b && b <= n).then(|| s.chars().skip(a).take(b - a).collect())
}

/// Whitespace collapsed to single spaces.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
    fn the_anchor_is_the_spans_text_by_characters() {
        let src: Vec<String> = [
            "pub fn unicode(a: u8) -> u8 {",
            "    let _s = \"éé—\"; if a < 10 { a + 1 } else { 0 }",
            "}",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
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
    fn files_are_named_as_the_record_keeps_them() {
        let d = checkout();
        let repo = d.path();
        let s = Sources::new(repo, None).unwrap();
        assert_eq!(
            s.resolve("src/lib.rs").unwrap(),
            ("src/lib.rs".into(), true)
        );
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
        let fork = Sources::new(repo, Some("vendor/w/crates/f/Cargo.toml")).unwrap();
        assert_eq!(
            fork.resolve("crates/f/src/lib.rs").unwrap(),
            ("vendor/w/crates/f/src/lib.rs".into(), true)
        );
        assert_eq!(
            fork.resolve("src/lib.rs").unwrap(),
            ("vendor/w/crates/f/src/lib.rs".into(), true)
        );
        assert_eq!(
            s.resolve("vendor/w/crates/f/src/../../f/src/lib.rs")
                .unwrap(),
            ("vendor/w/crates/f/src/lib.rs".into(), true)
        );
        assert_eq!(
            fork.resolve("crates/f/src/../../f/src/lib.rs").unwrap(),
            ("vendor/w/crates/f/src/lib.rs".into(), true)
        );
        for bad in [
            "/opt/elsewhere/x.rs",
            "src/missing.rs",
            "../src/lib.rs",
            "src/../../src/lib.rs",
            "src/./lib.rs",
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
    fn a_report_reduces_to_its_record() {
        let d = checkout();
        let rec = reduce_text(&probe_report(), d.path()).unwrap();
        assert_eq!((rec.functions, rec.total(), rec.proved()), (2, 3, 2));
        let o = &rec.rows[0];
        assert_eq!(o.function, "d27probe::twice");
        assert_eq!(o.anchor, "a + 1");
        assert_eq!(
            (o.file.as_str(), o.span.to_string()),
            ("src/lib.rs", "2:24-2:29".to_string())
        );
        assert_eq!(rec.rows[1].anchor, "x + x");
        let z = &rec.rows[2];
        assert_eq!(z.file, "@sysroot/alloc/src/macros.rs");
        assert_eq!(z.anchor, "-");
        assert!(z.desc.ends_with("(count `_7`)"), "{}", z.desc);
        let text = rec.to_text();
        let unit = format!("unit\t{}", rec.unit);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[..6],
            [
                RECORD_MAGIC,
                "package\td27probe",
                unit.as_str(),
                "functions\t2",
                "total\t3",
                "proved\t2",
            ]
        );
        let row: Vec<&str> = lines[6].split('\t').collect();
        assert_eq!(
            row,
            [
                "o",
                "d27probe::twice",
                "arithmetic_overflow_add",
                "src/lib.rs",
                "2",
                "24",
                "2",
                "29",
                "proved",
                "a + 1",
                "arithmetic overflow (Add)",
            ]
        );
        assert_eq!(lines.last().copied(), Some("end\t3"));
        let none = probe_report().replacen(
            r#""location":{"file":"src/lib.rs","line_start":3,"col_start":4,"line_end":3,"col_end":9},"#,
            "",
            1,
        );
        let rec = reduce_text(&none, d.path()).unwrap();
        let o = &rec.rows[1];
        assert_eq!((o.file.as_str(), o.anchor.as_str()), ("@none", "-"));
        assert_eq!(o.span.to_string(), "0:0-0:0");
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
}
