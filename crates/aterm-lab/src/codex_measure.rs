// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-measure [--package DIR] [--keep]`: MEASURE A CODEX-ALAB BUILD for aterm's rollout tap,
//! or refuse with one named class and leave the checkout as it was.
//!
//! Ruling T8: a codex build's rollout is read only once that build is measured, matched whole.
//! A build is measured when its six legs' rollouts are committed reduced in aterm-agent's step
//! testdata, since `CODEX_MEASURED_BUILDS` is the set of builds those reductions name
//! (aterm-agent's `build.rs`, which reads the files a checkout holds, so the verb reads the step
//! testdata only as HEAD holds it). The verb does what a person did by hand for 12672, 13172 and
//! 13594, except that it writes no golden: one written from its own fold would match by
//! construction, so a build it measures is held to its rollout's own account and to the keys and
//! record kinds every measured build showed.
//!
//! 1. the key: the build the package's `codex` runs as its rollouts name it ([`alab_build`]);
//!    NOT RUN when a codex reduction in the step testdata is not as HEAD holds it, such as a
//!    cut-short run's leftover ([`committed_in`]); exit 0, writing nothing, when HEAD holds all
//!    six of its legs; refuse STEM when it is no codex-alab build, when only some of its legs are
//!    committed, or when its reductions' names are another build's ([`admit`]);
//! 2. build aterm (`targo --unverified build -p aterm`), under [`LOCK`] from here to the end
//!    (each proof run holds the proof's own lock as well);
//! 3. run 1, the proof in process expecting `unmeasured` ([`codex_alab_proof::run`]); refuse
//!    PROOF-RED on any FAIL ([`gate`]);
//! 4. each leg's one rollout (ROLLOUTS), every `session_meta` naming the key (KEY), reduced twice
//!    and a third pass byte-identical ([`fixed_point`], FIXED-POINT), its own line naming the key
//!    (KEY) ([`reduce_leg`]);
//! 5. the keys and record kinds against every committed codex-alab reduction (NEW-KEY) and the
//!    rollout keys the previous build's same leg showed (GONE) ([`judge`]); other drift is noted;
//! 6. the six reductions written create-new, each path said as it is made;
//! 7. the standing tests ([`STANDING`]), the crates named (STANDING, [`standing_verdict`]);
//! 8. aterm rebuilt and run 2 expecting `bound` (NOT-BOUND);
//! 9. the note, `docs/measured/codex-alab-proof-<count>-<date>.md`, only when all passed;
//! 10. both worlds removed (kept and named on a refusal, or with `--keep`).
//!
//! A refusal after step 6, an interrupt or a panic removes exactly the files the run created
//! ([`landed`], and [`Written`]'s drop), and a second interrupt, which ends the process at once,
//! removes them first ([`undo_on_hard_exit`]). Only a kill leaves them, and step 1 of the next
//! run refuses them as leftovers. An interrupt is the run's (exit 128 + the signal), never a
//! step's refusal. What stays a person's or an agent's: the CHANGELOG line, the commit, the merge
//! and the push.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use aterm_agent::harness::codex_alab::{
    ALAB_BUILD_JSON, PACKAGE_JSON, alab_build, package_entrypoint,
};
use aterm_agent::harness::steps::StepEvent;
use aterm_json::Value;
use aterm_phase::anchors::{ROLLOUT_KEYS, rollout_reduction_stem};
use aterm_uds::spawnfd::Stdio;

use crate::codex_alab_proof::{self, Run};
use crate::instance::{take_flag, take_value};
use crate::paths::paths;
use crate::reduce::{self, Program};
use crate::stop::Stopped;
use crate::{Fail, LAB, Res};

/// The legs whose rollouts are measured, as their reductions are named (`<stem>-<leg>.jsonl`).
pub(crate) const LEGS: [&str; 6] = [
    "first", "continue", "approval", "question", "usage", "carry",
];

/// The step testdata the reductions are committed in, below the checkout.
pub(crate) const STEPS: &str = "crates/aterm-agent/src/harness/testdata/steps";

/// Where the note goes, below the checkout.
const MEASURED: &str = "docs/measured";

/// The lock the verb holds from its build to its end, below the lab's state root: one measurement
/// on this Mac at a time.
const LOCK: &str = "codex-measure.lock";

/// The libtest filters step 7 runs: the codex step fold's tests, the anchors' (the rollout keys
/// and the reductions' names), the reducer's fixed point over every committed `codex-*` file, and
/// this verb's own, which read the step testdata with the new reductions in place.
const FILTERS: [&str; 4] = [
    "harness::codex_steps::",
    "anchors::tests::",
    "reduce::tests::a_tool_argument_name_is_never_kept_as_written",
    "codex_measure::tests::",
];

/// The standing tests step 7 must see pass, by their libtest names: the fold whole and `On`,
/// equal to the rollout's own account, a moved duration (NC4), the key lower bound, the names,
/// the fixed point with no text kept, and every codex-alab build in the step testdata read as
/// measured, its six legs whole.
pub(crate) const STANDING: [&str; 7] = [
    "harness::codex_steps::tests::every_committed_codex_reduction_folds_whole",
    "harness::codex_steps::tests::the_codex_alab_legs_fold_exact",
    "harness::codex_steps::tests::a_moved_duration_fails_the_exact_golden",
    "anchors::tests::every_rollout_key_is_in_its_committed_rollout",
    "anchors::tests::a_rollout_reduction_is_named_by_its_build",
    "reduce::tests::a_tool_argument_name_is_never_kept_as_written",
    "codex_measure::tests::every_codex_alab_build_in_the_step_testdata_is_its_six_legs",
];

/// Why the verb refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// The build's reductions would take a name another build's hold, or it names no codex-alab
    /// stem.
    Stem,
    /// The package names no build, or a leg's rollout names another.
    Key,
    /// A leg wrote no rollout, or more than one.
    Rollouts,
    /// A reduction pass refused, or the third pass is not the second's bytes.
    FixedPoint,
    /// A key or record kind no measured build showed.
    NewKey,
    /// A rollout key the fold reads that the previous build's same leg showed.
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
            Class::FixedPoint => "FIXED-POINT",
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
    fn new(class: Class, why: impl Into<String>) -> Refusal {
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

/// One committed Codex rollout reduction: its file name, the build its own line names, its text.
#[derive(Debug)]
pub(crate) struct Reduction {
    pub name: String,
    pub build: Option<String>,
    pub text: String,
}

/// The build a reduction's own line names (`# codex <build> · lab reduce …`).
#[must_use]
pub(crate) fn header_build(text: &str) -> Option<&str> {
    text.strip_prefix("# codex ")?
        .split(' ')
        .next()
        .filter(|b| !b.is_empty() && *b != "-")
}

/// Whether `build` is a codex-alab build, as the reductions' names and aterm-agent's `build.rs`
/// read one.
#[must_use]
pub(crate) fn is_alab(build: &str) -> bool {
    rollout_reduction_stem(build).starts_with("codex-alab-")
}

/// Whether the file `name` is a Codex rollout reduction, as aterm-agent's `build.rs` reads one.
fn is_reduction(name: &str) -> bool {
    name.starts_with("codex-") && name.ends_with(".jsonl")
}

/// The `codex-*.jsonl` reductions in `steps`, by name: every such file there, committed or not
/// ([`committed_in`] holds them to HEAD).
/// # Errors
/// `steps` or a reduction cannot be read.
pub(crate) fn committed(steps: &Path) -> Result<Vec<Reduction>, String> {
    let entries = std::fs::read_dir(steps).map_err(|e| format!("{}: {e}", steps.display()))?;
    let mut out = Vec::new();
    for e in entries {
        let e = e.map_err(|e| format!("{}: {e}", steps.display()))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !is_reduction(&name) {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).map_err(|e| format!("{name}: {e}"))?;
        out.push(Reduction {
            build: header_build(&text).map(str::to_owned),
            name,
            text,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// STEP 1's reading of the step testdata in the checkout `repo`: its reductions ([`committed`]),
/// once git says every `codex-*.jsonl` file there is as HEAD holds it. aterm-agent's `build.rs`
/// lists whatever the files say, so a reduction no commit holds (a run cut short by a kill leaves
/// its fixtures untracked) would otherwise read as a measurement nobody made whole.
/// # Errors
/// NOT RUN: a reduction there is untracked, ignored, changed, staged or removed (each named), or
/// git or the directory cannot be read.
pub(crate) fn committed_in(repo: &Path) -> Result<Vec<Reduction>, Fail> {
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
                .any(|w| is_reduction(w.rsplit('/').next().unwrap_or(w)))
        })
        .collect();
    if !off.is_empty() {
        return Err(not_run(format!(
            "codex reductions in {STEPS} are not as HEAD holds them (a cut-short run's leftovers, or a hand's), and a build counts as measured only by committed ones; commit or remove them, then run again:\n{}",
            off.join("\n")
        )));
    }
    committed(&repo.join(STEPS)).map_err(not_run)
}

/// STEP 1 on `build` against the committed reductions: `Ok(None)` when they hold all six of its
/// legs ([`LEGS`]), each naming it (it is measured, and nothing is to be done), else the stem its
/// reductions take.
/// # Errors
/// STEM: `build` names no codex-alab stem; the reductions that name it are not all six legs; or
/// another build's reduction holds its stem (the stem drops the `+g<sha>`, so two builds of one
/// count collide).
pub(crate) fn admit(build: &str, committed: &[Reduction]) -> Result<Option<String>, Refusal> {
    let stem = rollout_reduction_stem(build);
    if !is_alab(build) {
        return Err(Refusal::new(
            Class::Stem,
            format!(
                "{build} is no codex-alab build (`<release>-dev.<count>+g<sha>`): its reductions would be {stem}-<leg>.jsonl"
            ),
        ));
    }
    let named: Vec<&str> = committed
        .iter()
        .filter(|r| r.build.as_deref() == Some(build))
        .map(|r| r.name.as_str())
        .collect();
    if !named.is_empty() {
        let missing: Vec<String> = LEGS
            .iter()
            .map(|leg| format!("{stem}-{leg}.jsonl"))
            .filter(|n| !named.contains(&n.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(None);
        }
        return Err(Refusal::new(
            Class::Stem,
            format!(
                "{build} is measured only in part: {} name it, and no {} does",
                named.join(", "),
                missing.join(", ")
            ),
        ));
    }
    let whole = format!("{stem}.jsonl");
    let leg = format!("{stem}-");
    let taken: Vec<String> = committed
        .iter()
        .filter(|r| r.name == whole || r.name.starts_with(&leg))
        .map(|r| format!("{} ({})", r.name, r.build.as_deref().unwrap_or("no build")))
        .collect();
    if taken.is_empty() {
        Ok(Some(stem))
    } else {
        Err(Refusal::new(
            Class::Stem,
            format!(
                "{build}'s reductions would be {stem}-<leg>.jsonl, which another build's hold: {}",
                taken.join(", ")
            ),
        ))
    }
}

/// The build `pkg`'s `codex` runs, as its rollouts name it ([`alab_build`]: the marker's
/// `client_version`, else the manifest's `version`), the key the tap's running-image check uses.
/// # Errors
/// KEY: the package names no build.
pub(crate) fn package_build(pkg: &Path) -> Result<String, Refusal> {
    let manifest = std::fs::read_to_string(pkg.join(PACKAGE_JSON)).unwrap_or_default();
    let exe = package_entrypoint(&manifest).map(|e| pkg.join(e));
    exe.as_deref().and_then(alab_build).ok_or_else(|| {
        Refusal::new(
            Class::Key,
            format!(
                "{} names no build ({ALAB_BUILD_JSON}'s client_version or its manifest's version)",
                pkg.display()
            ),
        )
    })
}

/// The rows of a rollout or reduction: its lines that are neither blank nor `#` lines.
fn rows(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// STEP 4: the one rollout a leg's codex wrote under its `CODEX_HOME` (`home`).
/// # Errors
/// ROLLOUTS: none, or more than one.
pub(crate) fn one_rollout(leg: &str, home: &Path) -> Result<PathBuf, Refusal> {
    let all = aterm_agent::harness::codex_rollout::rollouts_by_write(home);
    match all.as_slice() {
        [(path, _)] => Ok(path.clone()),
        _ => Err(Refusal::new(
            Class::Rollouts,
            format!(
                "{leg}: {} rollouts under {} where one is reduced ({})",
                all.len(),
                home.display(),
                all.iter()
                    .map(|(p, _)| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// STEP 4: every `session_meta` row of the rollout `text` names `build` (`cli_version`), and one
/// does: the tap's gate reads the rollout's own build too.
/// # Errors
/// KEY: a row names another build or none, or no row is a `session_meta`.
pub(crate) fn key_holds(leg: &str, text: &str, build: &str) -> Result<(), Refusal> {
    let mut metas = 0;
    for (n, row) in rows(text).enumerate() {
        let Ok(v) = aterm_json::from_str::<Value>(row) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        metas += 1;
        let said = v
            .get("payload")
            .and_then(|p| p.get("cli_version"))
            .and_then(Value::as_str);
        if said != Some(build) {
            return Err(Refusal::new(
                Class::Key,
                format!(
                    "{leg}: row {} names cli_version {} where the package's build is {build}",
                    n + 1,
                    said.unwrap_or("none")
                ),
            ));
        }
    }
    if metas == 0 {
        return Err(Refusal::new(
            Class::Key,
            format!("{leg}: no session_meta row names the build"),
        ));
    }
    Ok(())
}

/// What `lab reduce --program codex` writes: [`reduce::reduce`] held by [`reduce::check_written`].
/// # Errors
/// The reducer's refusal.
pub(crate) fn reduce_checked(source: &str) -> Result<String, String> {
    let r = reduce::reduce(source, Program::Codex)?;
    reduce::check_written(source, &r, Program::Codex)?;
    Ok(r.text)
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

/// What a rollout or reduction shows: every JSON object key at every depth, and every row's
/// record kind (`<type>/<payload.type>`, `-` for none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shows {
    pub keys: BTreeSet<String>,
    pub kinds: BTreeSet<String>,
}

impl Shows {
    /// What `text` shows.
    /// # Errors
    /// A row is no JSON value.
    pub(crate) fn of(text: &str) -> Result<Shows, String> {
        let mut s = Shows::default();
        s.add(text)?;
        Ok(s)
    }

    /// Add what `text` shows.
    /// # Errors
    /// A row is no JSON value.
    pub(crate) fn add(&mut self, text: &str) -> Result<(), String> {
        for (n, row) in rows(text).enumerate() {
            let v: Value = aterm_json::from_str(row).map_err(|e| format!("row {}: {e}", n + 1))?;
            keys_of(&v, &mut self.keys);
            let word = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("-").to_string();
            self.kinds.insert(format!(
                "{}/{}",
                word(v.get("type")),
                word(v.get("payload").and_then(|p| p.get("type")))
            ));
        }
        Ok(())
    }
}

fn keys_of(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                out.insert(k.clone());
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

/// The steps the fold makes of `text`, each by its shape (`call <tool>`, `wall <kind>`, else its
/// kind), the bind left out.
#[must_use]
pub(crate) fn step_shapes(text: &str) -> Vec<String> {
    reduce::readings(text, Program::Codex)
        .steps
        .iter()
        .filter_map(|s| match &s.ev {
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
        })
        .collect()
}

/// What step 5 found in one leg: what refuses (`new`: NEW-KEY, `gone`: GONE) and what the note
/// lists.
#[derive(Debug, Default)]
pub(crate) struct Drift {
    pub new: Vec<String>,
    pub gone: Vec<String>,
    pub noted: Vec<String>,
}

/// STEP 5 for one leg's reduction `text`: against `union` (what every committed codex-alab
/// reduction shows), and against `prev`, the previous build's same leg (its build and text).
/// # Errors
/// A row is no JSON value.
pub(crate) fn drift(
    leg: &str,
    text: &str,
    union: &Shows,
    prev: Option<(&str, &str)>,
) -> Result<Drift, String> {
    let now = Shows::of(text).map_err(|e| format!("{leg}: {e}"))?;
    let mut d = Drift::default();
    d.new.extend(
        now.keys
            .difference(&union.keys)
            .map(|k| format!("{leg}: key {k:?}")),
    );
    d.new.extend(
        now.kinds
            .difference(&union.kinds)
            .map(|k| format!("{leg}: record {k}")),
    );
    let Some((build, before)) = prev else {
        return Ok(d);
    };
    for a in ROLLOUT_KEYS {
        let quoted = format!("\"{}\"", a.text);
        if before.contains(&quoted) && !text.contains(&quoted) {
            d.gone.push(format!("{leg}: {} ({build} showed it)", a.id));
        }
    }
    let was = Shows::of(before).map_err(|e| format!("{leg} of {build}: {e}"))?;
    let read = |k: &str| ROLLOUT_KEYS.iter().any(|a| a.text == k);
    d.noted.extend(
        was.keys
            .difference(&now.keys)
            .filter(|k| !read(k))
            .map(|k| format!("{leg}: key {k:?} gone since {build}")),
    );
    d.noted.extend(
        was.kinds
            .difference(&now.kinds)
            .map(|k| format!("{leg}: record {k} gone since {build}")),
    );
    let (then, steps) = (step_shapes(before), step_shapes(text));
    if then != steps {
        d.noted.push(format!(
            "{leg}: steps `{}` where {build}'s were `{}`",
            steps.join(", "),
            then.join(", ")
        ));
    }
    Ok(d)
}

/// The release numbers and dev count of a codex-alab build, to order builds by.
fn order_of(build: &str) -> (Vec<u64>, u64) {
    let release = build.split_once('+').map_or(build, |(r, _)| r);
    let (version, count) = release.split_once("-dev.").unwrap_or((release, "0"));
    (
        version.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
        count.parse().unwrap_or(0),
    )
}

/// The committed codex-alab build step 5 holds `build` to leg by leg: the newest older one, else
/// the newest.
#[must_use]
pub(crate) fn previous(build: &str, committed: &[Reduction]) -> Option<String> {
    let mut builds: Vec<&str> = committed
        .iter()
        .filter_map(|r| r.build.as_deref())
        .filter(|b| is_alab(b) && *b != build)
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

/// What step 5 decided: the builds it read, the previous one, and what the note lists.
#[derive(Debug)]
pub(crate) struct Judged {
    pub builds: usize,
    pub prev: Option<String>,
    pub keys: usize,
    pub kinds: usize,
    pub noted: Vec<String>,
}

/// STEP 5 over every leg of `build`'s reductions (`legs`: leg and text) against the committed
/// codex-alab reductions.
/// # Errors
/// NEW-KEY (every new key and kind named, and anything gone), else GONE; or a row is no JSON
/// value (FIXED-POINT: the reduction is no file the reader reads).
pub(crate) fn judge(
    build: &str,
    legs: &[(&str, String)],
    committed: &[Reduction],
) -> Result<Judged, Refusal> {
    let alab: Vec<&Reduction> = committed
        .iter()
        .filter(|r| r.build.as_deref().is_some_and(is_alab))
        .collect();
    let mut union = Shows::default();
    for r in &alab {
        union
            .add(&r.text)
            .map_err(|e| Refusal::new(Class::FixedPoint, format!("{}: {e}", r.name)))?;
    }
    let builds: BTreeSet<&str> = alab.iter().filter_map(|r| r.build.as_deref()).collect();
    let prev = previous(build, committed);
    let prev_stem = prev.as_deref().map(rollout_reduction_stem);
    let (mut new, mut gone, mut noted) = (Vec::new(), Vec::new(), Vec::new());
    let mut shown = Shows::default();
    for (leg, text) in legs {
        let before = prev_stem.as_ref().and_then(|s| {
            let name = format!("{s}-{leg}.jsonl");
            alab.iter()
                .find(|r| r.name == name)
                .map(|r| r.text.as_str())
        });
        let d = drift(leg, text, &union, prev.as_deref().zip(before))
            .map_err(|e| Refusal::new(Class::FixedPoint, e))?;
        let _ = shown.add(text);
        new.extend(d.new);
        gone.extend(d.gone);
        noted.extend(d.noted);
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
                "{build} shows what no measured build showed ({} builds read): {}{also}",
                builds.len(),
                new.join(", ")
            ),
        ));
    }
    if !gone.is_empty() {
        return Err(Refusal::new(
            Class::Gone,
            format!(
                "{build} lost a rollout key the fold reads: {}",
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
    })
}

/// Every file a [`Written`] made and still holds (neither undone nor kept), for
/// [`undo_on_hard_exit`].
static MADE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn made() -> MutexGuard<'static, Vec<PathBuf>> {
    MADE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The verb's [`crate::stop::at_hard_exit`] act: a second interrupt ends the process at once,
/// past every drop, so this removes first every file a [`Written`] made and still holds.
fn undo_on_hard_exit() {
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
    fixtures: &[(PathBuf, String)],
    then: impl FnOnce(&mut Written) -> Result<T, Fail>,
) -> Result<(Vec<PathBuf>, T), Fail> {
    let mut w = Written::default();
    let step = || -> Result<T, Fail> {
        for (path, text) in fixtures {
            w.create(path, text.as_bytes()).map_err(Fail::die)?;
            println!("codex-measure: 6. wrote {} (create-new)", path.display());
        }
        then(&mut w)
    };
    match step() {
        Ok(v) => Ok((w.keep(), v)),
        Err(mut f) => {
            let n = w.paths.len();
            let left = w.undo();
            let said = if left.is_empty() {
                format!("removed the {n} files it wrote, so the checkout is as it was")
            } else {
                format!("COULD NOT REMOVE {}", left.join("; "))
            };
            f.msg = Some(match (f.msg, f.stopped) {
                (Some(m), _) => format!("{m}; {said}"),
                (None, Some(sig)) => format!("interrupted by signal {sig}; {said}"),
                (None, None) => said,
            });
            Err(f)
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
fn build_aterm(repo: &Path) -> Result<PathBuf, Fail> {
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
        "codex-measure: built {} ({:.0} s)",
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

/// STEP 7: the standing tests, the crates named (verify's change cone reaches no `aterm-phase`
/// from a testdata change): the [`STANDING`] names that passed, every one ([`standing_verdict`]).
/// An interrupt before they start ends the run there.
/// # Errors
/// The run's interrupt; STANDING: the run failed, or a standing test did not pass in it; NOT RUN:
/// it could not start.
fn standing(repo: &Path) -> Result<Vec<String>, Fail> {
    crate::stop::check()?;
    let t = std::time::Instant::now();
    let mut cmd = aterm_uds::spawnfd::command(targo());
    cmd.args([
        "--unverified",
        "test",
        "-p",
        "aterm-agent",
        "-p",
        "aterm-phase",
        "-p",
        "aterm-lab",
        "--lib",
        "--bins",
        "--",
    ])
    .args(FILTERS)
    .current_dir(repo)
    .stdin(Stdio::null())
    .stderr(Stdio::inherit());
    let out = cmd
        .output()
        .map_err(|e| not_run(format!("{}: {e}", targo().to_string_lossy())))?;
    let text = String::from_utf8_lossy(&out.stdout);
    print!("{text}");
    let standing = standing_verdict(crate::stop::check(), out.status, &text)?;
    println!(
        "codex-measure: 7. the standing tests passed ({} of their filters' tests, {:.0} s)",
        passed_tests(&text).len(),
        t.elapsed().as_secs_f64()
    );
    Ok(standing)
}

/// STEP 7's verdict on a test run that ended `status`, writing the libtest text `stdout`,
/// `stopped` being the run's interrupt state when it ended: the [`STANDING`] names, every one
/// seen passing.
/// # Errors
/// The run's interrupt, when one arrived while the tests ran (their end is then the run's, never
/// a standing test's failure); else STANDING: the run failed, or a standing test did not pass in
/// it.
pub(crate) fn standing_verdict(
    stopped: Result<(), Stopped>,
    status: ExitStatus,
    stdout: &str,
) -> Result<Vec<String>, Fail> {
    stopped?;
    let passed = passed_tests(stdout);
    let missing: Vec<&str> = STANDING
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
    Ok(STANDING.iter().map(|s| (*s).to_string()).collect())
}

fn not_run(why: String) -> Fail {
    Fail::exit(3, format!("NOT RUN: {why}"))
}

/// `git -C repo args…`'s trimmed stdout.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
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

/// A run's line in the note and the report.
fn run_line(n: u8, expect: &str, run: &Run) -> String {
    format!(
        "run {n} (expecting `{expect}`): {}, {} ok, {} FAIL; fences {}; socket to last check {}",
        if run.fails.is_empty() { "PASS" } else { "FAIL" },
        run.oks,
        run.fails.len(),
        if run.fences.is_empty() {
            "none".to_string()
        } else {
            run.fences.join(", ")
        },
        run.took
            .map_or_else(|| "-".to_string(), |d| format!("{} s", d.as_secs()))
    )
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

/// STEPS 3 AND 8: a proof run's verdict, `class` refused on any FAIL (PROOF-RED for run 1,
/// NOT-BOUND for run 2).
/// # Errors
/// `class`: a check failed, the failed checks named and the run's world kept.
pub(crate) fn gate(class: Class, run: &Run) -> Result<(), Fail> {
    if run.fails.is_empty() {
        Ok(())
    } else {
        Err(red(class, run))
    }
}

/// One reduced leg: its name, its reduction, the rollout it came from.
struct Leg {
    name: &'static str,
    text: String,
    from: PathBuf,
}

/// STEP 4 on one leg's rollout `source`: every `session_meta` names `build` ([`key_holds`]), its
/// fixed point by `by` ([`fixed_point`]), and that reduction's own line names `build` too, since
/// that line is what aterm-agent's `build.rs` lists.
/// # Errors
/// KEY: a row or the reduction's own line names another build or none; FIXED-POINT.
pub(crate) fn reduce_leg(
    leg: &str,
    source: &str,
    build: &str,
    by: &dyn Fn(&str) -> Result<String, String>,
) -> Result<String, Refusal> {
    key_holds(leg, source, build)?;
    let text = fixed_point(leg, source, by)?;
    if header_build(&text) != Some(build) {
        return Err(Refusal::new(
            Class::Key,
            format!(
                "{leg}: the reduction's own line names {} where the package's build is {build}",
                header_build(&text).unwrap_or("no build")
            ),
        ));
    }
    Ok(text)
}

/// STEP 4 over run 1's world `world`: each of [`LEGS`]' one rollout, reduced ([`reduce_leg`]).
fn reduce_legs(world: &Path, build: &str) -> Result<Vec<Leg>, Refusal> {
    let mut out = Vec::new();
    for leg in LEGS {
        let home = world.join("legs").join(leg).join("codex-home");
        let from = one_rollout(leg, &home)?;
        let source = std::fs::read_to_string(&from).map_err(|e| {
            Refusal::new(Class::Rollouts, format!("{leg}: {}: {e}", from.display()))
        })?;
        let text = reduce_leg(leg, &source, build, &reduce_checked)?;
        println!(
            "codex-measure: 4. {leg}: one rollout ({} bytes), cli_version {build}, a fixed point ({} bytes; pass 3 = pass 2)",
            source.len(),
            text.len()
        );
        out.push(Leg {
            name: leg,
            text,
            from,
        });
    }
    Ok(out)
}

/// The package record's line in the note.
fn package_line(pkg: &Path) -> String {
    let record = std::fs::read_to_string(pkg.join(ALAB_BUILD_JSON))
        .ok()
        .and_then(|t| aterm_json::from_str::<Value>(&t).ok())
        .unwrap_or_default();
    let s = |path: &[&str]| {
        let mut v = Some(&record);
        for k in path {
            v = v.and_then(|v| v.get(k));
        }
        v.and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| "?".to_string())
    };
    let short = |t: String| t.chars().take(12).collect::<String>();
    format!(
        "source `{}`, upstream `{}` plus {} commits (`{}`, {}), target `{}`, compiler `{}`, packager `{}`, built {}",
        short(s(&["source"])),
        s(&["upstream", "release"]),
        s(&["upstream", "commits_since_release"]),
        short(s(&["upstream", "commit"])),
        s(&["upstream", "commit_date"]),
        s(&["target"]),
        s(&["compiler", "rustc"]),
        short(s(&["packager"])),
        s(&["built_at"]),
    )
}

/// What the note says.
struct Note<'a> {
    build: &'a str,
    pkg: &'a Path,
    /// The package's path as the note says it (`~/…` below the home).
    pkg_said: &'a str,
    date: &'a str,
    head: (&'a str, &'a str),
    runs: [&'a Run; 2],
    legs: &'a [Leg],
    stem: &'a str,
    judged: &'a Judged,
    passed: &'a [String],
}

impl Note<'_> {
    fn text(&self) -> String {
        let release = self.build.split_once('+').map_or(self.build, |(r, _)| r);
        let (commit, tree) = self.head;
        let mut t = format!(
            "<!-- SPDX-License-Identifier: Apache-2.0 -->\n<!-- Copyright 2026 Andrew Yates -->\n\
             # codex-alab {release}: measured for the session log ({date})\n\n\
             `{LAB} codex-measure` measured `{build}` on this Mac: `lab codex-alab-proof` twice in \
             process, first expecting the build unmeasured, then measured, with its six legs' rollouts \
             reduced from run 1's world between the runs. This page is the verb's own record.\n\n\
             ## What ran\n\n\
             * **The package:** `{build}` at `{pkg_said}`: {package}.\n\
             * **The aterm:** debug builds (`targo --unverified build -p aterm`) of commit `{commit}` \
             (tree `{tree}`): run 1 as committed, run 2 with the six reductions below added.\n\n\
             ## The runs\n\n",
            date = self.date,
            build = self.build,
            pkg_said = self.pkg_said,
            package = package_line(self.pkg),
        );
        for (n, (run, expect)) in self.runs.iter().zip(["unmeasured", "bound"]).enumerate() {
            t.push_str(&format!("* {}\n", run_line(n as u8 + 1, expect, run)));
        }
        t.push_str(&format!(
            "\n## The build measured\n\n\
             Each leg's one rollout under its scratch `CODEX_HOME` names `{build}` in every \
             `session_meta`. It was reduced (`lab reduce --program codex`, its check of what it writes \
             included), then reduced again; a third pass gave the second's bytes, so the second is a \
             fixed point. They are committed as `{STEPS}/{stem}-<leg>.jsonl`:\n\n\
             | leg | bytes | rows | sha256 |\n|---|---|---|---|\n",
            build = self.build,
            stem = self.stem,
        ));
        for leg in self.legs {
            t.push_str(&format!(
                "| {} | {} | {} | `{}` |\n",
                leg.name,
                leg.text.len(),
                rows(&leg.text).count(),
                aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(leg.text.as_bytes()))
            ));
        }
        let j = self.judged;
        t.push_str(&format!(
            "\n**Rollout keys.** The six legs show {} keys and {} record kinds, none that the {} \
             measured builds' reductions do not show, and no rollout key the fold reads that {}'s \
             same legs showed is gone.",
            j.keys,
            j.kinds,
            j.builds,
            j.prev.as_deref().unwrap_or("a previous build")
        ));
        if j.noted.is_empty() {
            t.push_str(" No other key, record kind or step shape moved.\n");
        } else {
            t.push_str(" What moved and does not refuse:\n\n");
            for n in &j.noted {
                t.push_str(&format!("* {n}\n"));
            }
        }
        t.push_str(&format!(
            "\n**The standing tests passed** with the reductions in place (`targo --unverified test \
             -p aterm-agent -p aterm-phase -p aterm-lab --lib --bins -- {}`): {}.\n",
            FILTERS.join(" "),
            self.passed
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        t
    }
}

/// `codex-measure [--package DIR] [--keep]`.
/// # Errors
/// Usage; NOT RUN (no package, a dirty checkout, the lock held, a build that failed); a refusal by
/// class; an interrupt.
pub fn cmd_codex_measure(args: &[String]) -> Res {
    crate::stop::install();
    crate::stop::at_hard_exit(undo_on_hard_exit);
    let mut a = args.to_vec();
    let keep = take_flag(&mut a, "--keep");
    let given = take_value(&mut a, "--package")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "codex-measure: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let p = paths();
    let repo = p.repo.clone();
    let steps = repo.join(STEPS);
    let home = p.need_store_home().map_err(not_run)?;
    let pkg = codex_alab_proof::package(given.as_deref(), home).map_err(not_run)?;
    let pkg_said = pkg.strip_prefix(home).map_or_else(
        |_| pkg.display().to_string(),
        |below| format!("~/{}", below.display()),
    );

    let build = package_build(&pkg)?;
    let committed = committed_in(&repo)?;
    let Some(stem) = admit(&build, &committed)? else {
        println!(
            "codex-measure: 1. {build} ({}) is measured already: HEAD holds its six legs' reductions, so CODEX_MEASURED_BUILDS names it; nothing written",
            pkg.display()
        );
        return Ok(0);
    };
    println!(
        "codex-measure: 1. {build} ({}) is not measured; its reductions will be {stem}-<leg>.jsonl",
        pkg.display()
    );
    let dirty = git(&repo, &["status", "--porcelain"]).map_err(not_run)?;
    if !dirty.is_empty() {
        return Err(not_run(format!(
            "{} has changes, so what it writes could not be told from them and its note could name no commit:\n{dirty}",
            repo.display()
        )));
    }
    let lock = crate::paths::state_root(crate::sys::getuid()).join(LOCK);
    let _held = crate::aos_learn::lock_within(&lock, Duration::ZERO, &|_| Ok(()))
        .map_err(|e| not_run(format!("another codex-measure is running: {e}")))?;
    let head = git(&repo, &["rev-parse", "HEAD"]).map_err(not_run)?;
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).map_err(not_run)?;
    let (head, tree) = (&head[..head.len().min(10)], &tree[..tree.len().min(10)]);

    println!("codex-measure: 2. building aterm at {head}");
    let aterm = build_aterm(&repo)?;
    println!("codex-measure: 3. run 1, expecting {build} unmeasured");
    let mut run1 = codex_alab_proof::run(aterm, &pkg, Some(false))?;
    println!("codex-measure: 3. {}", run_line(1, "unmeasured", &run1));
    gate(Class::ProofRed, &run1)?;
    let world1 = run1.world.dir.clone();
    let kept = |w: &Path| format!("; run 1's world is kept at {}", w.display());
    let legs = reduce_legs(&world1, &build).map_err(|r| {
        let mut f = Fail::from(r);
        f.msg = f.msg.map(|m| m + &kept(&world1));
        f
    })?;
    let texts: Vec<(&str, String)> = legs.iter().map(|l| (l.name, l.text.clone())).collect();
    let judged = judge(&build, &texts, &committed).map_err(|r| {
        let mut f = Fail::from(r);
        f.msg = f.msg.map(|m| m + &kept(&world1));
        f
    })?;
    println!(
        "codex-measure: 5. {} keys and {} record kinds, nothing new against {} measured builds' reductions, no rollout key gone against {}{}",
        judged.keys,
        judged.kinds,
        judged.builds,
        judged.prev.as_deref().unwrap_or("no previous build"),
        if judged.noted.is_empty() {
            "; nothing else moved".to_string()
        } else {
            format!("; noted: {}", judged.noted.join("; "))
        }
    );
    for leg in &legs {
        println!("    {} from {}", leg.name, leg.from.display());
    }

    let fixtures: Vec<(PathBuf, String)> = legs
        .iter()
        .map(|l| {
            (
                steps.join(format!("{stem}-{}.jsonl", l.name)),
                l.text.clone(),
            )
        })
        .collect();
    let date = {
        let (y, m, d, ..) = crate::sys::local_time(crate::util::now_secs());
        format!("{y:04}-{m:02}-{d:02}")
    };
    let count = order_of(&build).1;
    let note_path = repo
        .join(MEASURED)
        .join(format!("codex-alab-proof-{count}-{date}.md"));
    let (written, (passed, mut run2)) = landed(&fixtures, |w| {
        let passed = standing(&repo).map_err(|f| with_world(f, &world1, None))?;
        println!("codex-measure: 8. rebuilding aterm with the reductions");
        let aterm = build_aterm(&repo).map_err(|f| with_world(f, &world1, None))?;
        println!("codex-measure: 8. run 2, expecting {build} bound");
        let run2 = codex_alab_proof::run(aterm, &pkg, Some(true))
            .map_err(|f| with_world(f, &world1, None))?;
        println!("codex-measure: 8. {}", run_line(2, "bound", &run2));
        gate(Class::NotBound, &run2).map_err(|f| with_world(f, &world1, None))?;
        let note = Note {
            build: &build,
            pkg: &pkg,
            pkg_said: &pkg_said,
            date: &date,
            head: (head, tree),
            runs: [&run1, &run2],
            legs: &legs,
            stem: &stem,
            judged: &judged,
            passed: &passed,
        };
        w.create(&note_path, note.text().as_bytes())
            .map_err(|e| with_world(not_run(e), &world1, Some(&run2.world.dir)))?;
        println!("codex-measure: 9. wrote {}", note_path.display());
        Ok((passed, run2))
    })?;
    if keep {
        println!(
            "codex-measure: 10. the worlds are kept (--keep): {} and {}",
            world1.display(),
            run2.world.dir.display()
        );
    } else {
        println!(
            "codex-measure: 10. removing the worlds {} and {}",
            world1.display(),
            run2.world.dir.display()
        );
        run1.world.release();
        run2.world.release();
    }
    println!(
        "MEASURED {build}: {} tests passed; wrote {}",
        passed.len(),
        written
            .iter()
            .map(|p| p.strip_prefix(&repo).unwrap_or(p).display().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "left to do by hand: a CHANGELOG line under [Unreleased], the commit, the merge and the push"
    );
    Ok(0)
}

/// `f` naming the worlds kept for it.
fn with_world(mut f: Fail, run1: &Path, run2: Option<&Path>) -> Fail {
    if f.stopped.is_some() {
        return f;
    }
    let worlds = match run2 {
        Some(w) => format!(
            "; the worlds are kept at {} and {}",
            run1.display(),
            w.display()
        ),
        None => format!("; run 1's world is kept at {}", run1.display()),
    };
    f.msg = Some(f.msg.unwrap_or_default() + &worlds);
    f
}

#[cfg(test)]
#[path = "codex_measure_tests.rs"]
mod tests;
