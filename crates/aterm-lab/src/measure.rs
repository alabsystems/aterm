// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STEPS `lab codex-measure` AND `lab claude-measure` SHARE, so a program's build is measured
//! one way whichever program it is: the step testdata read only as HEAD holds it
//! ([`committed_in`]), one reducer's fixed point ([`fixed_point`], [`reduce_leg`]), one judge of
//! the keys and record kinds against every committed reduction of the program ([`judge`], then
//! [`exercised`] once run 2 passed), the create-new writes a refusal removes ([`landed`],
//! [`Written`]), the aterm build and the standing tests. A [`Family`] says what differs: the
//! program, which files are its reductions, a row's record kind and build, the keys its fold
//! reads, the row kinds it does not act on, and the standing tests.
//!
//! THE JUDGE'S RULE (ruling 2026-10-28, fix J2-4), for a key or record kind no measured build
//! showed: on a row kind the fold does not act on ([`Family::inert`]) it is NOTED, since such a row
//! changes nothing the fold makes (aterm-agent's law test
//! `an_inert_row_changes_nothing_the_fold_makes`); a key the fold reads ([`Family::read`]) on a row
//! it acts on is REFUSED (NEW-KEY), a format change the fold must be taught; any other key, and a
//! new record kind of a row the fold acts on, is ADMITTED only once run 2, expecting the build
//! exact, wrote rows carrying it and passed every check, and the verdict names the run and the
//! check; else it is REFUSED (NEW-KEY) as one no run exercised. A key is held by the row kinds a
//! measured build showed it on, not by its name: one shown only on rows the fold does not act on
//! is new the first time a build writes it on a row the fold acts on, and GONE holds a build only
//! to what the previous build showed on rows the fold acts on ([`drift`]). The privacy oracle runs
//! before the judge on each reduction it reads, run 2's included (`claude_measure::text_kept`,
//! `codex_measure::text_kept`).
//!
//! Ruling T8: a build is read exact only once it is measured, and it is measured when its
//! reductions are committed in aterm-agent's step testdata, since `build.rs` lists the builds
//! their own lines name. So each verb reads that testdata only as HEAD holds it, writes nothing
//! until a build has passed every step before the write, and removes what it wrote on any later
//! refusal.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use aterm_agent::harness::steps::StepEvent;
use aterm_json::Value;
use aterm_phase::anchors::Anchor;
use aterm_uds::spawnfd::Stdio;

use crate::Fail;
use crate::codex_alab_proof::Run;
use crate::reduce::{self, Program};
use crate::stop::Stopped;

/// The step testdata the reductions are committed in, below the checkout.
pub(crate) const STEPS: &str = "crates/aterm-agent/src/harness/testdata/steps";

/// Where a measurement's note goes, below the checkout.
pub(crate) const MEASURED: &str = "docs/measured";

/// The lock a measurement holds from its build to its end, below the lab's state root: one
/// measurement on this Mac at a time, of either program, since each reads the step testdata the
/// other writes.
const LOCK: &str = "measure.lock";

/// What one program's measurement reads and holds, where the verbs differ.
pub(crate) struct Family {
    /// The program, whose name a reduction's own line opens with (`# <program> <build> · lab
    /// reduce`).
    pub program: Program,
    /// The verb, as its lines open (`codex-measure`).
    pub verb: &'static str,
    /// What the program writes, as a refusal names it (`rollout`, `transcript`).
    pub source: &'static str,
    /// The keys and closed words the program's step fold reads, as the anchors name them (the
    /// fold reads each through `anchor!`): GONE holds a build to them, and NEW-KEY refuses a new
    /// one. Each family's tests hold the table to the fold itself: a key whose removal from a
    /// committed reduction changes what the fold makes of it must be here
    /// (`keys_the_fold_reads`), and aterm-agent's step tests hold `TRANSCRIPT_KEYS` to the keys
    /// the fold's projections keep (`the_fold_reads_what_the_transcript_keys_name`).
    pub read: &'static [Anchor],
    /// Whether the fold acts on no row of the record kind `kind` (`<type>/<…>`): a new key or
    /// record kind there is only noted.
    pub inert: fn(&str) -> bool,
    /// Whether the file `name` in the step testdata is one of its reductions, as aterm-agent's
    /// `build.rs` reads one.
    pub holds: fn(&str) -> bool,
    /// Whether the build a reduction names is a measured build a new one is held to (Codex: a
    /// codex-alab build).
    pub measures: fn(&str) -> bool,
    /// A row's record kind (`<type>/<…>`, `-` for none).
    pub kind: fn(&Value) -> String,
    /// The reduction `leg` of `build` is committed as.
    pub name_of: fn(&str, &str) -> String,
    /// Every row of the source `text` of `leg` that names a build names `build`, and one does.
    pub key_holds: fn(&str, &str, &str) -> Result<(), Refusal>,
    /// The crates step 7 tests.
    pub crates: &'static [&'static str],
    /// The libtest filters step 7 runs.
    pub filters: &'static [&'static str],
    /// The standing tests step 7 must see pass, by their libtest names.
    pub standing: &'static [&'static str],
}

/// Why a measurement refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// The build's reductions would take a name another build's hold, or it names no codex-alab
    /// stem.
    Stem,
    /// The program names no build, or a row or a reduction's own line names another.
    Key,
    /// A leg wrote no rollout, or more than one.
    Rollouts,
    /// A session wrote no transcript, or more than one.
    Transcripts,
    /// A reduction pass refused, or the third pass is not the second's bytes.
    FixedPoint,
    /// The reduction keeps text of what it reduced (the privacy oracle, ruling T7).
    Text,
    /// A key the fold reads that no measured build showed, on a row it acts on; or a new key or
    /// record kind of a row it acts on that no run exercised.
    NewKey,
    /// A key the fold reads that the previous build's same leg showed.
    Gone,
    /// Run 1 failed a check.
    ProofRed,
    /// A standing test failed or did not run.
    Standing,
    /// Run 2 failed a check.
    NotBound,
}

impl Class {
    /// Its word in a refusal.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Class::Stem => "STEM",
            Class::Key => "KEY",
            Class::Rollouts => "ROLLOUTS",
            Class::Transcripts => "TRANSCRIPTS",
            Class::FixedPoint => "FIXED-POINT",
            Class::Text => "TEXT",
            Class::NewKey => "NEW-KEY",
            Class::Gone => "GONE",
            Class::ProofRed => "PROOF-RED",
            Class::Standing => "STANDING",
            Class::NotBound => "NOT-BOUND",
        }
    }
}

/// A refusal: its class and what was seen.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub class: Class,
    pub why: String,
}

impl Refusal {
    pub(crate) fn new(class: Class, why: impl Into<String>) -> Refusal {
        Refusal {
            class,
            why: why.into(),
        }
    }
}

impl From<Refusal> for Fail {
    fn from(r: Refusal) -> Fail {
        Fail::die(format!("REFUSED {}: {}", r.class.word(), r.why))
    }
}

/// One committed reduction: its file name, the build its own line names, its text.
#[derive(Debug)]
pub(crate) struct Reduction {
    pub name: String,
    pub build: Option<String>,
    pub text: String,
}

/// The build a `program` reduction's own line names (`# <program> <build> · lab reduce …`), as
/// aterm-agent's `build.rs` reads it.
#[must_use]
pub(crate) fn header_build(program: Program, text: &str) -> Option<&str> {
    text.strip_prefix("# ")?
        .strip_prefix(program.name())?
        .strip_prefix(' ')?
        .split(' ')
        .next()
        .filter(|b| !b.is_empty() && *b != "-")
}

/// `f`'s reductions in `steps`, by name: every such file there, committed or not
/// ([`committed_in`] holds them to HEAD).
/// # Errors
/// `steps` or a reduction cannot be read.
pub(crate) fn committed(f: &Family, steps: &Path) -> Result<Vec<Reduction>, String> {
    let entries = std::fs::read_dir(steps).map_err(|e| format!("{}: {e}", steps.display()))?;
    let mut out = Vec::new();
    for e in entries {
        let e = e.map_err(|e| format!("{}: {e}", steps.display()))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !(f.holds)(&name) {
            continue;
        }
        let text = crate::util::read_regular_text(&e.path()).map_err(|e| format!("{name}: {e}"))?;
        out.push(Reduction {
            build: header_build(f.program, &text).map(str::to_owned),
            name,
            text,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// STEP 1's reading of the step testdata in the checkout `repo`: `f`'s reductions
/// ([`committed`]), once git says every one of its files there is as HEAD holds it. aterm-agent's
/// `build.rs` lists whatever the files say, so a reduction no commit holds (a run cut short by a
/// kill leaves its fixtures untracked) would otherwise read as a measurement nobody made whole.
/// # Errors
/// NOT RUN: a reduction there is untracked, ignored, changed, staged or removed (each named), or
/// git or the directory cannot be read.
pub(crate) fn committed_in(f: &Family, repo: &Path) -> Result<Vec<Reduction>, Fail> {
    let said = git(
        repo,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored",
            "--",
            STEPS,
        ],
    )
    .map_err(not_run)?;
    let off: Vec<&str> = said
        .lines()
        .map(str::trim)
        .filter(|l| {
            l.split(' ')
                .any(|w| (f.holds)(w.rsplit('/').next().unwrap_or(w)))
        })
        .collect();
    if !off.is_empty() {
        return Err(not_run(format!(
            "{} reductions in {STEPS} are not as HEAD holds them (a cut-short run's leftovers, or a hand's), and a build counts as measured only by committed ones; commit or remove them, then run again:\n{}",
            f.program.name(),
            off.join("\n")
        )));
    }
    committed(f, &repo.join(STEPS)).map_err(not_run)
}

/// The rows of a source or reduction: its lines that are neither blank nor `#` lines.
pub(crate) fn rows(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// What `lab reduce --program <program>` writes: [`reduce::reduce`] held by
/// [`reduce::check_written`].
pub(crate) fn reduce_checked(program: Program) -> impl Fn(&str) -> Result<String, String> {
    move |source| {
        let r = reduce::reduce(source, program)?;
        reduce::check_written(source, &r, program)?;
        Ok(r.text)
    }
}

/// STEP 4: pass 1 and pass 2 of `source` by `by` (each pass checked by it), then pass 3, which
/// must give pass 2's bytes: pass 2.
/// # Errors
/// FIXED-POINT: a pass refused, or pass 3 is not pass 2.
pub(crate) fn fixed_point(
    leg: &str,
    source: &str,
    by: &dyn Fn(&str) -> Result<String, String>,
) -> Result<String, Refusal> {
    let pass = |n: u8, text: &str| {
        by(text).map_err(|e| Refusal::new(Class::FixedPoint, format!("{leg}: pass {n}: {e}")))
    };
    let second = pass(2, &pass(1, source)?)?;
    let third = pass(3, &second)?;
    if third == second {
        return Ok(second);
    }
    let line = second
        .lines()
        .zip(third.lines())
        .position(|(a, b)| a != b)
        .map_or_else(|| "past the shorter".to_string(), |n| (n + 1).to_string());
    Err(Refusal::new(
        Class::FixedPoint,
        format!(
            "{leg}: pass 3 is not pass 2 ({} and {} bytes, first apart at line {line})",
            second.len(),
            third.len()
        ),
    ))
}

/// STEP 4 on one leg's source `source`: every row that names a build names `build`
/// ([`Family::key_holds`]), its fixed point by `by` ([`fixed_point`]), and that reduction's own
/// line names `build` too, since that line is what aterm-agent's `build.rs` lists.
/// # Errors
/// KEY: a row or the reduction's own line names another build or none; FIXED-POINT.
pub(crate) fn reduce_leg(
    f: &Family,
    leg: &str,
    source: &str,
    build: &str,
    by: &dyn Fn(&str) -> Result<String, String>,
) -> Result<String, Refusal> {
    (f.key_holds)(leg, source, build)?;
    let text = fixed_point(leg, source, by)?;
    let named = header_build(f.program, &text);
    if named != Some(build) {
        return Err(Refusal::new(
            Class::Key,
            format!(
                "{leg}: the reduction's own line names {} where the build measured is {build}",
                named.unwrap_or("no build")
            ),
        ));
    }
    Ok(text)
}

/// What a source or reduction shows: every JSON object key at every depth but the names the
/// reducer gives keys (`k<n>`, which hold nothing of the program's), every row's record kind
/// ([`Family::kind`]), and the record kinds of the rows that carry each key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shows {
    pub keys: BTreeSet<String>,
    pub kinds: BTreeSet<String>,
    pub on: BTreeMap<String, BTreeSet<String>>,
}

impl Shows {
    /// What `text` shows.
    /// # Errors
    /// A row is no JSON value.
    pub(crate) fn of(f: &Family, text: &str) -> Result<Shows, String> {
        let mut s = Shows::default();
        s.add(f, text)?;
        Ok(s)
    }

    /// Whether a row of a record kind the fold acts on ([`Family::inert`]) carries `key`.
    #[must_use]
    pub(crate) fn acted_on(&self, f: &Family, key: &str) -> bool {
        self.on
            .get(key)
            .is_some_and(|kinds| kinds.iter().any(|kind| !(f.inert)(kind)))
    }

    /// Add what `text` shows.
    /// # Errors
    /// A row is no JSON value.
    pub(crate) fn add(&mut self, f: &Family, text: &str) -> Result<(), String> {
        for (n, row) in rows(text).enumerate() {
            let v: Value = aterm_json::from_str(row).map_err(|e| format!("row {}: {e}", n + 1))?;
            let kind = (f.kind)(&v);
            let mut keys = BTreeSet::new();
            keys_of(&v, &mut keys);
            for k in &keys {
                self.on.entry(k.clone()).or_default().insert(kind.clone());
            }
            self.keys.extend(keys);
            self.kinds.insert(kind);
        }
        Ok(())
    }
}

/// Whether the JSON row `v` carries the key `key` at any depth (never a reducer's `k<n>`).
fn carries(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, v)| (k == key && !reduce::is_key_name(k)) || carries(v, key)),
        Value::Array(a) => a.iter().any(|v| carries(v, key)),
        _ => false,
    }
}

fn keys_of(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                if !reduce::is_key_name(k) {
                    out.insert(k.clone());
                }
                keys_of(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                keys_of(v, out);
            }
        }
        _ => {}
    }
}

/// The keys of `text`'s rows that `f`'s step fold reads, as the fold itself says: those whose
/// removal from every object of every row changes the steps it makes of them, what it lost or
/// dropped, or the chain's end after each row. Both readings are of the rows serialized again,
/// so only the key differs. GONE holds a build only to the keys [`Family::read`] names, so each
/// of these must be one of them.
/// # Errors
/// A row is no JSON value.
#[cfg(test)]
pub(crate) fn keys_the_fold_reads(f: &Family, text: &str) -> Result<BTreeSet<String>, String> {
    fn without(v: &mut Value, key: &str) {
        match v {
            Value::Object(m) => {
                m.remove(key);
                for v in m.values_mut() {
                    without(v, key);
                }
            }
            Value::Array(a) => {
                for v in a {
                    without(v, key);
                }
            }
            _ => {}
        }
    }
    let parsed = rows(text)
        .enumerate()
        .map(|(n, row)| {
            aterm_json::from_str::<Value>(row).map_err(|e| format!("row {}: {e}", n + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let fold = |drop: Option<&str>| {
        let mut again = String::new();
        for v in &parsed {
            let mut v = v.clone();
            if let Some(k) = drop {
                without(&mut v, k);
            }
            again.push_str(&aterm_json::to_string(&v).expect("a parsed row serializes"));
            again.push('\n');
        }
        let r = reduce::readings(&again, f.program);
        (r.steps, r.lost, r.overflow, r.end, r.ends)
    };
    let base = fold(None);
    Ok(Shows::of(f, text)?
        .keys
        .into_iter()
        .filter(|k| fold(Some(k)) != base)
        .collect())
}

/// Every key the step fold of `f` reads of a committed reduction in `steps` ([`keys_the_fold_reads`])
/// that [`Family::read`] does not name, with the reductions that show it: what GONE would let a
/// build drop unrefused.
#[cfg(test)]
pub(crate) fn read_but_not_held(f: &Family, steps: &Path) -> Vec<String> {
    let mut missed: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for r in committed(f, steps).expect("the step testdata") {
        for k in keys_the_fold_reads(f, &r.text).expect("a committed reduction is JSON rows") {
            if !f.read.iter().any(|a| a.text == k) {
                missed.entry(k).or_default().push(r.name.clone());
            }
        }
    }
    missed
        .into_iter()
        .map(|(k, names)| format!("{k:?} ({})", names.join(", ")))
        .collect()
}

/// The word at `v` (`-` for none), for a record kind.
#[must_use]
pub(crate) fn word(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("-").to_string()
}

/// A step by its shape (`call <tool>`, `wall <kind>`, else its kind), a bind none.
fn shape_of(ev: &StepEvent) -> Option<String> {
    match ev {
        StepEvent::Bind { .. } => None,
        StepEvent::Prompt { .. } => Some("prompt".to_string()),
        StepEvent::Interrupt => Some("interrupt".to_string()),
        StepEvent::ToolUse { tool, .. } => Some(format!("call {}", tool.as_str())),
        StepEvent::ToolResult { .. } => Some("result".to_string()),
        StepEvent::TurnEnd { .. } => Some("end".to_string()),
        StepEvent::EndWall { kind } => Some(format!("wall {kind:?}")),
        StepEvent::Compact => Some("compact".to_string()),
        StepEvent::Model { .. } => Some("model".to_string()),
        StepEvent::Off { .. } => Some("off".to_string()),
    }
}

/// The steps `program`'s fold makes of `text`, each by its shape ([`shape_of`]), the bind left
/// out.
#[must_use]
pub(crate) fn step_shapes(program: Program, text: &str) -> Vec<String> {
    reduce::readings(text, program)
        .steps
        .iter()
        .filter_map(|s| shape_of(&s.ev))
        .collect()
}

/// The steps `program`'s fold makes of row `row` (0-based among its rows) of `text`, as a verdict
/// says them (`result`, `call Bash, result`), or `no step`.
#[must_use]
pub(crate) fn row_steps(program: Program, text: &str, row: usize) -> String {
    let made: Vec<String> = reduce::readings(text, program)
        .steps
        .iter()
        .filter(|s| s.row == row)
        .filter_map(|s| shape_of(&s.ev))
        .collect();
    if made.is_empty() {
        "no step".to_string()
    } else {
        made.join(", ")
    }
}

/// A new key, or a new record kind (`key` none), on rows of the record kind `kind` the fold acts
/// on and does not read the key of: admitted only once run 2 exercised it ([`exercised`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub leg: String,
    pub kind: String,
    pub key: Option<String>,
}

impl Pending {
    /// How a verdict names it: `<leg>: key "<key>" on <kind>`, or `<leg>: record <kind>`.
    #[must_use]
    pub(crate) fn said(&self) -> String {
        match &self.key {
            Some(k) => format!("{}: key {k:?} on {}", self.leg, self.kind),
            None => format!("{}: record {}", self.leg, self.kind),
        }
    }
}

/// What step 5 found in one leg: what refuses (`new`: NEW-KEY, `gone`: GONE), what run 2 must
/// exercise (`pending`), and what the note lists.
#[derive(Debug, Default)]
pub(crate) struct Drift {
    pub new: Vec<String>,
    pub pending: Vec<Pending>,
    pub gone: Vec<String>,
    pub noted: Vec<String>,
}

/// STEP 5 for one leg's reduction `text`: against `union` (what every committed reduction of a
/// measured build shows), and against `prev`, the previous build's same leg (its build and text).
/// A key is new to the fold when no measured build showed it on a row kind the fold acts on,
/// whatever rows it showed it on that the fold does not act on: a key first noted on an inert row
/// is still held to rules (b) and (c) the first time a build writes it where the fold acts. GONE
/// holds a build to the keys the fold reads that the previous build showed on rows it acts on,
/// and only to those rows.
/// # Errors
/// A row is no JSON value.
pub(crate) fn drift(
    f: &Family,
    leg: &str,
    text: &str,
    union: &Shows,
    prev: Option<(&str, &str)>,
) -> Result<Drift, String> {
    let now = Shows::of(f, text).map_err(|e| format!("{leg}: {e}"))?;
    let read = |k: &str| f.read.iter().any(|a| a.text == k);
    let mut d = Drift::default();
    for (k, kinds) in &now.on {
        let (inert, acted): (Vec<&String>, Vec<&String>) =
            kinds.iter().partition(|kind| (f.inert)(kind));
        if !acted.is_empty() && !union.acted_on(f, k) {
            let before = if union.keys.contains(k) {
                ", shown before only on rows the fold does not act on"
            } else {
                ""
            };
            if read(k) {
                d.new.push(format!(
                    "{leg}: key {k:?} on {}, a key the fold reads{before}",
                    join(&acted)
                ));
            } else {
                d.pending.extend(acted.iter().map(|kind| Pending {
                    leg: leg.to_string(),
                    kind: (*kind).clone(),
                    key: Some(k.clone()),
                }));
            }
        }
        if !inert.is_empty() && !union.keys.contains(k) {
            d.noted.push(format!(
                "{leg}: key {k:?} on {}, rows the fold does not act on",
                join(&inert)
            ));
        }
    }
    for kind in now.kinds.difference(&union.kinds) {
        if (f.inert)(kind) {
            d.noted.push(format!(
                "{leg}: record {kind}, a row the fold does not act on"
            ));
        } else {
            d.pending.push(Pending {
                leg: leg.to_string(),
                kind: kind.clone(),
                key: None,
            });
        }
    }
    let Some((build, before)) = prev else {
        return Ok(d);
    };
    let (was_acted, now_acted) = (
        acted_rows(f, before).map_err(|e| format!("{leg} of {build}: {e}"))?,
        acted_rows(f, text).map_err(|e| format!("{leg}: {e}"))?,
    );
    let mut lost = BTreeSet::new();
    for a in f.read {
        let quoted = format!("\"{}\"", a.text);
        if was_acted.contains(&quoted) && !now_acted.contains(&quoted) {
            lost.insert(a.text);
            d.gone.push(format!("{leg}: {} ({build} showed it)", a.id));
        }
    }
    let was = Shows::of(f, before).map_err(|e| format!("{leg} of {build}: {e}"))?;
    d.noted.extend(
        was.keys
            .difference(&now.keys)
            .filter(|k| !lost.contains(k.as_str()))
            .map(|k| format!("{leg}: key {k:?} gone since {build}")),
    );
    d.noted.extend(
        was.kinds
            .difference(&now.kinds)
            .map(|k| format!("{leg}: record {k} gone since {build}")),
    );
    let (then, steps) = (step_shapes(f.program, before), step_shapes(f.program, text));
    if then != steps {
        d.noted.push(format!(
            "{leg}: steps `{}` where {build}'s were `{}`",
            steps.join(", "),
            then.join(", ")
        ));
    }
    Ok(d)
}

/// The rows of `text` of a record kind the fold acts on ([`Family::inert`]), one a line: what
/// GONE reads a key the fold reads in, since a row of any other kind changes nothing it makes.
/// # Errors
/// A row is no JSON value.
fn acted_rows(f: &Family, text: &str) -> Result<String, String> {
    let mut out = String::new();
    for (n, row) in rows(text).enumerate() {
        let v: Value = aterm_json::from_str(row).map_err(|e| format!("row {}: {e}", n + 1))?;
        if !(f.inert)(&(f.kind)(&v)) {
            out.push_str(row);
            out.push('\n');
        }
    }
    Ok(out)
}

/// A build's release numbers and dev count (`<release>[-dev.<count>][+g<sha>]`), to order builds
/// by.
#[must_use]
pub(crate) fn order_of(build: &str) -> (Vec<u64>, u64) {
    let release = build.split_once('+').map_or(build, |(r, _)| r);
    let (version, count) = release.split_once("-dev.").unwrap_or((release, "0"));
    (
        version.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
        count.parse().unwrap_or(0),
    )
}

/// The committed measured build step 5 holds `build` to leg by leg: the newest older one, else
/// the newest.
#[must_use]
pub(crate) fn previous(f: &Family, build: &str, committed: &[Reduction]) -> Option<String> {
    let mut builds: Vec<&str> = committed
        .iter()
        .filter_map(|r| r.build.as_deref())
        .filter(|b| (f.measures)(b) && *b != build)
        .collect();
    builds.sort_by_key(|b| order_of(b));
    builds.dedup();
    let at = order_of(build);
    builds
        .iter()
        .rev()
        .find(|b| order_of(b) < at)
        .or_else(|| builds.last())
        .map(|b| (*b).to_string())
}

/// The names of `kinds`, joined.
fn join(kinds: &[&String]) -> String {
    kinds
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// What step 5 decided: the builds it read, the previous one, what the note lists, and what run 2
/// must exercise.
#[derive(Debug)]
pub(crate) struct Judged {
    pub builds: usize,
    pub prev: Option<String>,
    pub keys: usize,
    pub kinds: usize,
    pub noted: Vec<String>,
    pub pending: Vec<Pending>,
}

/// STEP 5 over every leg of `build`'s reductions (`legs`: leg and text) against the committed
/// reductions of `f`'s measured builds, by the judge's rule (the module's head): what is new on a
/// row the fold does not act on is noted, what it reads refuses, and the rest waits on run 2
/// ([`Judged::pending`], [`exercised`]).
/// # Errors
/// NEW-KEY (every new key the fold reads named, and anything gone), else GONE; or a row is no JSON
/// value (FIXED-POINT: the reduction is no file the reader reads).
pub(crate) fn judge(
    f: &Family,
    build: &str,
    legs: &[(&str, String)],
    committed: &[Reduction],
) -> Result<Judged, Refusal> {
    let measured: Vec<&Reduction> = committed
        .iter()
        .filter(|r| r.build.as_deref().is_some_and(f.measures))
        .collect();
    let mut union = Shows::default();
    for r in &measured {
        union
            .add(f, &r.text)
            .map_err(|e| Refusal::new(Class::FixedPoint, format!("{}: {e}", r.name)))?;
    }
    let builds: BTreeSet<&str> = measured.iter().filter_map(|r| r.build.as_deref()).collect();
    let prev = previous(f, build, committed);
    let (mut new, mut gone, mut noted, mut pending) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut shown = Shows::default();
    for (leg, text) in legs {
        let before = prev.as_deref().and_then(|p| {
            let name = (f.name_of)(p, leg);
            measured
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.text.as_str())
        });
        let d = drift(f, leg, text, &union, prev.as_deref().zip(before))
            .map_err(|e| Refusal::new(Class::FixedPoint, e))?;
        let _ = shown.add(f, text);
        new.extend(d.new);
        gone.extend(d.gone);
        noted.extend(d.noted);
        pending.extend(d.pending);
    }
    if !new.is_empty() {
        let also = if gone.is_empty() {
            String::new()
        } else {
            format!("; and gone: {}", gone.join(", "))
        };
        return Err(Refusal::new(
            Class::NewKey,
            format!(
                "{build} shows a key the fold reads that no measured build showed ({} builds read): {}{also}",
                builds.len(),
                new.join(", ")
            ),
        ));
    }
    if !gone.is_empty() {
        return Err(Refusal::new(
            Class::Gone,
            format!(
                "{build} lost a {} key the fold reads: {}",
                f.source,
                gone.join(", ")
            ),
        ));
    }
    Ok(Judged {
        builds: builds.len(),
        prev,
        keys: shown.keys.len(),
        kinds: shown.kinds.len(),
        noted,
        pending,
    })
}

/// THE JUDGE'S RULE (b), once `run` (run 2, expecting the build exact) passed every check: each of
/// `pending` admitted by the first row of its record kind in `run`'s reduction of its leg (`legs`:
/// leg and reduction, each held to the privacy oracle as run 1's was) that carries its key, or for
/// a new record kind is of it. What admitted each, by `check_of` (the leg, the reduction and the
/// row, 0-based among its rows: the run's check that read the step the fold made of that row):
/// one line per key, naming the run and the check.
/// # Errors
/// NEW-KEY: no row of `run`'s carries one (each named, with its row kind, as one no run
/// exercised); FIXED-POINT: a row is no JSON value.
pub(crate) fn exercised(
    f: &Family,
    build: &str,
    pending: &[Pending],
    run: &str,
    legs: &[(&str, String)],
    check_of: &dyn Fn(&str, &str, usize) -> String,
) -> Result<Vec<String>, Refusal> {
    let (mut admitted, mut missed) = (Vec::new(), Vec::new());
    for p in pending {
        let Some((_, text)) = legs.iter().find(|(leg, _)| *leg == p.leg) else {
            missed.push(format!("{} ({run} wrote no {} leg)", p.said(), p.leg));
            continue;
        };
        let mut at = None;
        for (n, row) in rows(text).enumerate() {
            let v: Value = aterm_json::from_str(row).map_err(|e| {
                Refusal::new(Class::FixedPoint, format!("{}: row {}: {e}", p.leg, n + 1))
            })?;
            if (f.kind)(&v) == p.kind && p.key.as_deref().is_none_or(|k| carries(&v, k)) {
                at = Some(n);
                break;
            }
        }
        match at {
            Some(n) => admitted.push(format!(
                "{}: {run}, row {} ({})",
                p.said(),
                n + 1,
                check_of(&p.leg, text, n)
            )),
            None => missed.push(format!(
                "{} (no run exercised it: {run} wrote no {} row {})",
                p.said(),
                p.kind,
                if p.key.is_some() {
                    "carrying it"
                } else {
                    "at all"
                }
            )),
        }
    }
    if missed.is_empty() {
        Ok(admitted)
    } else {
        Err(Refusal::new(
            Class::NewKey,
            format!(
                "{build} shows on rows the fold acts on what no measured build showed and no run exercised: {}",
                missed.join(", ")
            ),
        ))
    }
}

/// Every file a [`Written`] made and still holds (neither undone nor kept), for
/// [`undo_on_hard_exit`].
static MADE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub(crate) fn made() -> MutexGuard<'static, Vec<PathBuf>> {
    MADE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A measurement's [`crate::stop::at_hard_exit`] act: a second interrupt ends the process at
/// once, past every drop, so this removes first every file a [`Written`] made and still holds.
pub(crate) fn undo_on_hard_exit() {
    for p in made().drain(..) {
        let _ = std::fs::remove_file(&p);
    }
}

/// The files a run created, each create-new, so a refusal removes exactly them. Dropped while it
/// holds any (a panic unwinding, an early return), it removes them; [`Written::keep`] takes them
/// as the run's result.
#[derive(Debug, Default)]
pub(crate) struct Written {
    pub paths: Vec<PathBuf>,
}

impl Written {
    /// Create `path` (never over a file) holding `bytes`.
    /// # Errors
    /// It exists, or it cannot be written (what was made of it stays listed, to be removed).
    pub(crate) fn create(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let mut f = {
            // Listed for the hard exit under the same lock as the open, so a second interrupt
            // meets every file this made.
            let mut made = made();
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| format!("{}: {e} (never written over)", path.display()))?;
            made.push(path.to_path_buf());
            f
        };
        self.paths.push(path.to_path_buf());
        f.write_all(bytes)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// No longer list its files for [`undo_on_hard_exit`].
    fn forget(&self) {
        made().retain(|p| !self.paths.contains(p));
    }

    /// Remove every file it created: what could not be removed, said.
    pub(crate) fn undo(&mut self) -> Vec<String> {
        let left = self
            .paths
            .iter()
            .filter_map(|p| {
                std::fs::remove_file(p)
                    .err()
                    .map(|e| format!("{}: {e}", p.display()))
            })
            .collect();
        self.forget();
        self.paths.clear();
        left
    }

    /// Keep every file it created, the run's result: neither its drop nor a hard exit removes
    /// them.
    pub(crate) fn keep(mut self) -> Vec<PathBuf> {
        self.forget();
        std::mem::take(&mut self.paths)
    }
}

impl Drop for Written {
    fn drop(&mut self) {
        let _ = self.undo();
    }
}

/// STEPS 6 TO 9: create `fixtures` (path and text), each path said as it is made, then `then`
/// (the standing tests, the second run and the note, which writes through the same [`Written`]);
/// on any failure there, an interrupt among them, remove exactly the files created, so the
/// checkout ends as it began (a panic does too, by the drop). The files it wrote, kept.
/// # Errors
/// A fixture cannot be created, or `then`'s failure, saying what was removed.
pub(crate) fn landed<T>(
    f: &Family,
    fixtures: &[(PathBuf, String)],
    then: impl FnOnce(&mut Written) -> Result<T, Fail>,
) -> Result<(Vec<PathBuf>, T), Fail> {
    let mut w = Written::default();
    let step = || -> Result<T, Fail> {
        for (path, text) in fixtures {
            w.create(path, text.as_bytes()).map_err(Fail::die)?;
            println!("{}: 6. wrote {} (create-new)", f.verb, path.display());
        }
        then(&mut w)
    };
    match step() {
        Ok(v) => Ok((w.keep(), v)),
        Err(mut fail) => {
            let n = w.paths.len();
            let left = w.undo();
            let said = if left.is_empty() {
                format!("removed the {n} files it wrote, so the checkout is as it was")
            } else {
                format!("COULD NOT REMOVE {}", left.join("; "))
            };
            fail.msg = Some(match (fail.msg, fail.stopped) {
                (Some(m), _) => format!("{m}; {said}"),
                (None, Some(sig)) => format!("interrupted by signal {sig}; {said}"),
                (None, None) => said,
            });
            Err(fail)
        }
    }
}

/// The cargo that runs the lab (`$CARGO`: the lab's own PATH holds no toolchain).
fn targo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "targo".into())
}

/// The aterm binary a `--message-format=json` build's lines name.
#[must_use]
pub(crate) fn aterm_artifact(json_lines: &str) -> Option<PathBuf> {
    json_lines
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("reason").and_then(Value::as_str) == Some("compiler-artifact"))
        .filter(|v| {
            v.get("target")
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                == Some("aterm")
        })
        .filter_map(|v| {
            v.get("executable")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .next_back()
}

/// STEPS 2 AND 8: `targo --unverified build -p aterm` in the checkout `repo`, the binary it built
/// ([`built`]). An interrupt before it starts ends the run there.
/// # Errors
/// The run's interrupt; NOT RUN: the build failed, or named no aterm.
pub(crate) fn build_aterm(f: &Family, repo: &Path) -> Result<PathBuf, Fail> {
    crate::stop::check()?;
    let t = std::time::Instant::now();
    let out = aterm_uds::spawnfd::command(targo())
        .args([
            "--unverified",
            "build",
            "-p",
            "aterm",
            "--message-format=json",
        ])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| not_run(format!("{}: {e}", targo().to_string_lossy())))?;
    let aterm = built(
        crate::stop::check(),
        out.status,
        &String::from_utf8_lossy(&out.stdout),
    )?;
    println!(
        "{}: built {} ({:.0} s)",
        f.verb,
        aterm.display(),
        t.elapsed().as_secs_f64()
    );
    Ok(aterm)
}

/// What a build of aterm that ended `status`, writing the JSON lines `stdout`, gave, `stopped`
/// being the run's interrupt state when it ended: the binary it names.
/// # Errors
/// The run's interrupt, when one arrived while the build ran (the build's end is then the
/// run's, never a failed build); else NOT RUN: the build failed, or named no aterm.
pub(crate) fn built(
    stopped: Result<(), Stopped>,
    status: ExitStatus,
    stdout: &str,
) -> Result<PathBuf, Fail> {
    stopped?;
    if !status.success() {
        return Err(not_run(format!(
            "`targo --unverified build -p aterm` exited {status}"
        )));
    }
    aterm_artifact(stdout).ok_or_else(|| not_run("the build named no aterm binary".to_string()))
}

/// The libtest lines `test <name> ... ok` of `out`, by name.
#[must_use]
pub(crate) fn passed_tests(out: &str) -> BTreeSet<String> {
    out.lines()
        .filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... ok"))
        .map(str::to_owned)
        .collect()
}

/// STEP 7: the standing tests, the crates `f` names (verify's change cone reaches no
/// `aterm-phase` from a testdata change): the [`Family::standing`] names that passed, every one
/// ([`standing_verdict`]). An interrupt before they start ends the run there.
/// # Errors
/// The run's interrupt; STANDING: the run failed, or a standing test did not pass in it; NOT RUN:
/// it could not start.
pub(crate) fn standing(f: &Family, repo: &Path) -> Result<Vec<String>, Fail> {
    crate::stop::check()?;
    let t = std::time::Instant::now();
    let mut cmd = aterm_uds::spawnfd::command(targo());
    cmd.args(["--unverified", "test"]);
    for c in f.crates {
        cmd.args(["-p", c]);
    }
    cmd.args(["--lib", "--bins", "--"])
        .args(f.filters)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    let out = cmd
        .output()
        .map_err(|e| not_run(format!("{}: {e}", targo().to_string_lossy())))?;
    let text = String::from_utf8_lossy(&out.stdout);
    print!("{text}");
    let standing = standing_verdict(f, crate::stop::check(), out.status, &text)?;
    println!(
        "{}: 7. the standing tests passed ({} of their filters' tests, {:.0} s)",
        f.verb,
        passed_tests(&text).len(),
        t.elapsed().as_secs_f64()
    );
    Ok(standing)
}

/// The command line step 7 runs, as a note says it.
#[must_use]
pub(crate) fn standing_line(f: &Family) -> String {
    let crates: Vec<String> = f.crates.iter().map(|c| format!("-p {c}")).collect();
    format!(
        "targo --unverified test {} --lib --bins -- {}",
        crates.join(" "),
        f.filters.join(" ")
    )
}

/// STEP 7's verdict on a test run that ended `status`, writing the libtest text `stdout`,
/// `stopped` being the run's interrupt state when it ended: the [`Family::standing`] names, every
/// one seen passing.
/// # Errors
/// The run's interrupt, when one arrived while the tests ran (their end is then the run's, never
/// a standing test's failure); else STANDING: the run failed, or a standing test did not pass in
/// it.
pub(crate) fn standing_verdict(
    f: &Family,
    stopped: Result<(), Stopped>,
    status: ExitStatus,
    stdout: &str,
) -> Result<Vec<String>, Fail> {
    stopped?;
    let passed = passed_tests(stdout);
    let missing: Vec<&str> = f
        .standing
        .iter()
        .copied()
        .filter(|n| !passed.contains(*n))
        .collect();
    if !status.success() || !missing.is_empty() {
        let failed: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED"))
            .collect();
        return Err(Refusal::new(
            Class::Standing,
            format!(
                "the standing tests exited {status} (failed: {}; did not pass: {})",
                if failed.is_empty() {
                    "none named".to_string()
                } else {
                    failed.join(", ")
                },
                if missing.is_empty() {
                    "none".to_string()
                } else {
                    missing.join(", ")
                }
            ),
        )
        .into());
    }
    Ok(f.standing.iter().map(|s| (*s).to_string()).collect())
}

/// A step that could not run (exit 3).
pub(crate) fn not_run(why: String) -> Fail {
    Fail::exit(3, format!("NOT RUN: {why}"))
}

/// `git -C repo args…`'s trimmed stdout.
pub(crate) fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What a measurement checks of the checkout `repo` and holds before it builds: no change in it
/// (what the run writes must be told from it, and its note names a commit), the one lock
/// ([`LOCK`]), and HEAD's commit and tree, each cut to ten digits.
/// # Errors
/// NOT RUN: the checkout has changes, another measurement holds the lock, or git cannot be read.
pub(crate) fn hold_clean(
    f: &Family,
    repo: &Path,
) -> Result<(crate::aos_learn::Held, String, String), Fail> {
    let dirty = git(repo, &["status", "--porcelain"]).map_err(not_run)?;
    if !dirty.is_empty() {
        return Err(not_run(format!(
            "{} has changes, so what it writes could not be told from them and its note could name no commit:\n{dirty}",
            repo.display()
        )));
    }
    let lock = crate::paths::state_root(crate::sys::getuid()).join(LOCK);
    let held = crate::aos_learn::lock_within(&lock, Duration::ZERO, &|_| Ok(()))
        .map_err(|e| not_run(format!("another measurement is running ({}): {e}", f.verb)))?;
    let head = git(repo, &["rev-parse", "HEAD"]).map_err(not_run)?;
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]).map_err(not_run)?;
    let cut = |s: &str| s[..s.len().min(10)].to_string();
    Ok((held, cut(&head), cut(&tree)))
}

/// Today's local date, as a note's name and heading say it (`2026-10-28`).
#[must_use]
pub(crate) fn today() -> String {
    let (y, m, d, ..) = crate::sys::local_time(crate::util::now_secs());
    format!("{y:04}-{m:02}-{d:02}")
}

/// The refusal of a run's FAIL: its class, the failed checks, its world kept.
fn red(class: Class, run: &Run) -> Fail {
    Refusal::new(
        class,
        format!(
            "{} check(s) failed ({}); its world is kept at {}",
            run.fails.len(),
            run.fails.join("; "),
            run.world.dir.display()
        ),
    )
    .into()
}

/// STEPS 3 AND 8: a run's verdict, `class` refused on any FAIL (PROOF-RED for run 1, NOT-BOUND
/// for run 2).
/// # Errors
/// `class`: a check failed, the failed checks named and the run's world kept.
pub(crate) fn gate(class: Class, run: &Run) -> Result<(), Fail> {
    if run.fails.is_empty() {
        Ok(())
    } else {
        Err(red(class, run))
    }
}

/// `fail` naming the worlds kept for it.
pub(crate) fn with_world(mut fail: Fail, run1: &Path, run2: Option<&Path>) -> Fail {
    if fail.stopped.is_some() {
        return fail;
    }
    let worlds = match run2 {
        Some(w) => format!(
            "; the worlds are kept at {} and {}",
            run1.display(),
            w.display()
        ),
        None => format!("; run 1's world is kept at {}", run1.display()),
    };
    fail.msg = Some(fail.msg.unwrap_or_default() + &worlds);
    fail
}

/// A git repository at `repo` holding the step testdata directory with `files` (name and text),
/// one commit in it: what both verbs' tests read [`committed_in`] on.
#[cfg(test)]
pub(crate) fn repo_with(repo: &Path, files: &[(String, String)]) {
    git(repo, &["init", "-q"]).expect("git init");
    let steps = repo.join(STEPS);
    std::fs::create_dir_all(&steps).expect("the steps dir");
    for (name, text) in files {
        std::fs::write(steps.join(name), text).expect("a reduction");
    }
    git(repo, &["add", "--", STEPS]).expect("git add");
    git(
        repo,
        &[
            "-c",
            "user.name=lab",
            "-c",
            "user.email=lab@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "the step testdata",
        ],
    )
    .expect("git commit");
}

/// The sha256 of `text`, as a note's table says it.
#[must_use]
pub(crate) fn sha256(text: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(text.as_bytes()))
}
