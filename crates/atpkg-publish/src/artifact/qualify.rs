// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A product's typed qualification list: `[workspace.metadata.atpkg] qualify` in its own
//! repository (beside `expose` and `build-args`), and the runner that holds a frozen source and
//! its packed artifact to it.
//!
//! ```toml
//! [[workspace.metadata.atpkg.qualify]]
//! name = "fmt"
//! stage = "source"
//! run = ["targo", "fmt", "--check"]
//! within-secs = 120
//!
//! [[workspace.metadata.atpkg.qualify]]
//! name = "unit"
//! stage = "source"
//! run = ["targo", "--unverified", "test", "--locked", "-p", "ay-core"]
//! within-secs = 1800
//! min-tests = 40
//!
//! [[workspace.metadata.atpkg.qualify]]
//! name = "build"
//! stage = "build"
//! within-secs = 3600
//!
//! [[workspace.metadata.atpkg.qualify]]
//! name = "version"
//! stage = "packed"
//! run = ["{bin}/ay", "--version"]
//! within-secs = 30
//! ```
//!
//! The order is the order the steps run in: the cheap source checks, then the one build (the
//! pack itself, built with `build-args`), then the controls run on the packed binaries. Each
//! step commits its own time bound. A step passes only when it exits 0 within its bound and,
//! where it ran libtest, ran at least one test: a run that matched nothing is a failure. A
//! command is ended at its bound. The build's bound is given to the pack, which ends its build
//! at it, and is judged again when the pack returns, so a pack that overran is refused.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use super::tools::{HostTools, Ran, Run, RunError};
use crate::error::{Error, Result};
use crate::triple::Triple;

/// The longest bound a step may commit.
pub const MAX_WITHIN_SECS: u64 = 24 * 3600;

/// The most steps one list may carry.
pub const MAX_STEPS: usize = 64;

/// The longest step name.
pub const MAX_NAME_BYTES: usize = 48;

/// The packed tree's root in a packed step's argv.
pub const ROOT: &str = "{root}";

/// The packed tree's `bin` directory in a packed step's argv.
pub const BIN: &str = "{bin}";

/// When a step runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// In the frozen checkout, before anything is built.
    Source,
    /// The pack's own build.
    Build,
    /// Against the unpacked asset.
    Packed,
}

impl Stage {
    /// `source` | `build` | `packed`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Build => "build",
            Self::Packed => "packed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "source" => Some(Self::Source),
            "build" => Some(Self::Build),
            "packed" => Some(Self::Packed),
            _ => None,
        }
    }
}

/// One declared step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Its name: unique in the list, and the stem of its log files.
    pub name: String,
    /// When it runs.
    pub stage: Stage,
    /// Its argv (empty for the build, which is the pack's own).
    pub run: Vec<String>,
    /// Its committed time bound.
    pub within: Duration,
    /// The fewest libtest tests that must pass, when it declares a floor.
    pub min_tests: Option<u64>,
}

/// A validated list: source steps, one build, then at least one packed control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    steps: Vec<Step>,
    build: usize,
}

impl Plan {
    /// Every step, in declared order.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The source checks, in order.
    #[must_use]
    pub fn source(&self) -> &[Step] {
        &self.steps[..self.build]
    }

    /// The build.
    #[must_use]
    pub fn build(&self) -> &Step {
        &self.steps[self.build]
    }

    /// The packed-binary controls, in order.
    #[must_use]
    pub fn packed(&self) -> &[Step] {
        &self.steps[self.build + 1..]
    }

    /// One line per step, as a plan prints it.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| {
                let what = if s.stage == Stage::Build {
                    "the pack's build".to_string()
                } else {
                    s.run.join(" ")
                };
                let floor = s
                    .min_tests
                    .map(|n| format!(", at least {n} tests"))
                    .unwrap_or_default();
                format!(
                    "{:<6} {} (within {} s{floor}): {what}",
                    s.stage.as_str(),
                    s.name,
                    s.within.as_secs()
                )
            })
            .collect()
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0] != b'-'
}

const KEYS: &[&str] = &["name", "stage", "run", "within-secs", "min-tests"];

fn step(value: &aterm_json::Value, at: usize, program: &str) -> Result<Step> {
    let refuse = |why: String| {
        Error::refused(format!(
            "{program}'s metadata.atpkg qualify step {}: {why}",
            at + 1
        ))
    };
    let table = value
        .as_object()
        .ok_or_else(|| refuse("must be a table".into()))?;
    if let Some(unknown) = table.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(refuse(format!(
            "`{unknown}` is not a step key ({}): a misspelt bound or floor is never dropped",
            KEYS.join(", ")
        )));
    }
    let name = value
        .get("name")
        .and_then(aterm_json::Value::as_str)
        .filter(|n| valid_name(n))
        .ok_or_else(|| {
            refuse(format!(
                "`name` must be 1 to {MAX_NAME_BYTES} of a-z, 0-9 and `-`, starting with a \
                 letter or digit"
            ))
        })?;
    let refuse = |why: String| {
        Error::refused(format!(
            "{program}'s metadata.atpkg qualify step `{name}`: {why}"
        ))
    };
    let stage = value
        .get("stage")
        .and_then(aterm_json::Value::as_str)
        .and_then(Stage::parse)
        .ok_or_else(|| refuse("`stage` must be \"source\", \"build\" or \"packed\"".into()))?;
    let within = value
        .get("within-secs")
        .and_then(aterm_json::Value::as_u64)
        .filter(|n| (1..=MAX_WITHIN_SECS).contains(n))
        .ok_or_else(|| {
            refuse(format!(
                "`within-secs` must be a whole number of seconds from 1 to {MAX_WITHIN_SECS}: \
                 every step commits its time bound"
            ))
        })?;
    let run = match value.get("run") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .and_then(|a| {
                a.iter()
                    .map(|s| s.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| refuse("`run` must be an array of nonempty strings".into()))?,
    };
    let min_tests = match value.get("min-tests") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n >= 1)
                .ok_or_else(|| refuse("`min-tests` must be a whole number, at least 1".into()))?,
        ),
    };
    match stage {
        Stage::Build => {
            if !run.is_empty() || min_tests.is_some() {
                return Err(refuse(
                    "the build is the pack's own (`build-args`): it takes `within-secs` only"
                        .into(),
                ));
            }
        }
        Stage::Source | Stage::Packed => {
            if run.is_empty() {
                return Err(refuse("`run` must name a command".into()));
            }
            if stage == Stage::Source
                && let Some(word) = run.iter().find(|w| w.contains(ROOT) || w.contains(BIN))
            {
                return Err(refuse(format!(
                    "`{word}` names the packed tree, which does not exist before the build; \
                     make it a \"packed\" step"
                )));
            }
            // A packed step runs from a scratch directory outside the checkout: one that names
            // neither placeholder cannot be running what was packed.
            if stage == Stage::Packed && !run.iter().any(|w| w.contains(ROOT) || w.contains(BIN)) {
                return Err(refuse(format!(
                    "a \"packed\" step must name the packed tree ({BIN} or {ROOT}) in `run`: \
                     a control that runs none of the package's files qualifies none of them"
                )));
            }
        }
    }
    Ok(Step {
        name: name.to_string(),
        stage,
        run,
        within: Duration::from_secs(within),
        min_tests,
    })
}

/// The qualification list `program` declares in `targo metadata --no-deps --format-version 1`'s
/// JSON: its own package's `metadata.atpkg` recipe when it has one, else the workspace's (the
/// precedence [`super::pack::parse_metadata`] gives `expose` and `build-args`). `None` when it
/// declares no list.
/// # Errors
/// A refusal naming the first thing wrong: JSON that is not `targo metadata`'s, a list that is
/// not an array of tables, an unknown key, a bad name, stage, argv, bound or test floor, a
/// repeated name, steps out of order (source, then exactly one build, then packed), no packed
/// control, or a packed control that names neither `{bin}` nor `{root}`.
pub fn parse(json: &[u8], program: &str) -> Result<Option<Plan>> {
    let v: aterm_json::Value = aterm_json::from_slice(json)
        .map_err(|e| Error::refused(format!("`targo metadata` printed no JSON: {e}")))?;
    let packages = v
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| Error::refused("`targo metadata` lists no packages"))?;
    let own = packages
        .iter()
        .find(|p| p.get("name").and_then(aterm_json::Value::as_str) == Some(program))
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("atpkg"));
    let recipe = own.or_else(|| v.get("metadata").and_then(|m| m.get("atpkg")));
    let Some(list) = recipe.and_then(|r| r.get("qualify")) else {
        return Ok(None);
    };
    let refuse = |why: String| Error::refused(format!("{program}'s metadata.atpkg qualify {why}"));
    let list = list
        .as_array()
        .ok_or_else(|| refuse("must be an array of step tables".into()))?;
    if list.is_empty() {
        return Err(refuse(
            "is empty: a list that checks nothing qualifies nothing".into(),
        ));
    }
    if list.len() > MAX_STEPS {
        return Err(refuse(format!(
            "carries {} steps, more than {MAX_STEPS}",
            list.len()
        )));
    }
    let steps = list
        .iter()
        .enumerate()
        .map(|(at, value)| step(value, at, program))
        .collect::<Result<Vec<_>>>()?;
    let mut names = BTreeSet::new();
    if let Some(twice) = steps.iter().find(|s| !names.insert(s.name.as_str())) {
        return Err(refuse(format!(
            "names `{}` twice: a step's name is its log's and its receipt's",
            twice.name
        )));
    }
    let builds: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.stage == Stage::Build)
        .map(|(i, _)| i)
        .collect();
    let &[build] = builds.as_slice() else {
        return Err(refuse(format!(
            "must carry exactly one \"build\" step, and carries {}",
            builds.len()
        )));
    };
    if let Some(late) = steps[build..].iter().find(|s| s.stage == Stage::Source) {
        return Err(refuse(format!(
            "runs source step `{}` after the build: the cheap source checks come first",
            late.name
        )));
    }
    if let Some(early) = steps[..build].iter().find(|s| s.stage == Stage::Packed) {
        return Err(refuse(format!(
            "runs packed step `{}` before the build, when there is nothing packed to run",
            early.name
        )));
    }
    if build + 1 == steps.len() {
        return Err(refuse(
            "carries no \"packed\" step: a package whose binaries nothing ran is not qualified"
                .into(),
        ));
    }
    Ok(Some(Plan { steps, build }))
}

/// What libtest reported across one step's output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tests {
    /// `test result:` lines read.
    pub summaries: u64,
    /// Tests that passed.
    pub passed: u64,
    /// Tests that failed.
    pub failed: u64,
}

impl Tests {
    /// Tests that ran.
    #[must_use]
    pub const fn ran(self) -> u64 {
        self.passed.saturating_add(self.failed)
    }
}

/// Sum every libtest summary (`test result: ok. 3 passed; 0 failed; …`) in `text`.
#[must_use]
pub fn count_tests(text: &str) -> Tests {
    let mut tests = Tests::default();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("test result: ") else {
            continue;
        };
        let count = |label: &str| {
            rest.split(';')
                .flat_map(|part| part.split(". "))
                .find_map(|part| part.trim().strip_suffix(label))
                .and_then(|n| n.trim().parse::<u64>().ok())
        };
        let (Some(passed), Some(failed)) = (count(" passed"), count(" failed")) else {
            continue;
        };
        tests.summaries += 1;
        tests.passed = tests.passed.saturating_add(passed);
        tests.failed = tests.failed.saturating_add(failed);
    }
    tests
}

/// How one step ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Exit 0 within its bound, with every test rule held.
    Passed,
    /// Why it is not a pass.
    Failed(String),
}

/// What one step's run left: the record a release receipt carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// The step.
    pub name: String,
    /// Its stage.
    pub stage: Stage,
    /// The target whose packed tree it ran on (a packed step).
    pub target: Option<Triple>,
    /// The argv it ran, placeholders resolved (empty for the build).
    pub argv: Vec<String>,
    /// Its exit code; `None` when it did not run to an exit.
    pub exit: Option<i32>,
    /// How long it took.
    pub millis: u64,
    /// Its committed bound.
    pub within_secs: u64,
    /// SHA-256 of its kept stdout: the bytes in its `.stdout` log (the runner keeps each
    /// stream's last [`aterm_uds::deadline::OUTPUT_KEEP`] bytes).
    pub stdout_sha256: String,
    /// SHA-256 of its kept stderr, likewise.
    pub stderr_sha256: String,
    /// What libtest reported in the kept output, when it printed a summary.
    pub tests: Option<Tests>,
    /// How it ended.
    pub verdict: Verdict,
}

impl Receipt {
    /// Whether the step passed.
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self.verdict, Verdict::Passed)
    }

    /// The build's record: the pack is the lane's own call, so its exit is 0 when it returned
    /// a package and 1 when it refused.
    #[must_use]
    pub fn build(step: &Step, elapsed: Duration, outcome: std::result::Result<(), String>) -> Self {
        let mut verdict = match &outcome {
            Ok(()) => Verdict::Passed,
            Err(why) => Verdict::Failed(why.clone()),
        };
        if outcome.is_ok() && elapsed > step.within {
            verdict = Verdict::Failed(over(step, elapsed));
        }
        Self {
            name: step.name.clone(),
            stage: Stage::Build,
            target: None,
            argv: Vec::new(),
            exit: Some(i32::from(outcome.is_err())),
            millis: millis(elapsed),
            within_secs: step.within.as_secs(),
            stdout_sha256: atpkg::tree::sha256_bytes(b""),
            stderr_sha256: atpkg::tree::sha256_bytes(b""),
            tests: None,
            verdict,
        }
    }

    /// One line: `PASS unit 12.3 s exit 0 (41 tests)` or `FAIL unit …: why`.
    #[must_use]
    pub fn line(&self) -> String {
        let on = self.target.map(|t| format!(" on {t}")).unwrap_or_default();
        let exit = self
            .exit
            .map_or_else(|| "no exit".to_string(), |c| format!("exit {c}"));
        let tests = self
            .tests
            .map(|t| format!(" ({} tests ran, {} passed)", t.ran(), t.passed))
            .unwrap_or_default();
        let head = format!(
            "{} {}{on} {}.{:03} s {exit}{tests}",
            self.stage.as_str(),
            self.name,
            self.millis / 1000,
            self.millis % 1000
        );
        match &self.verdict {
            Verdict::Passed => format!("PASS {head}"),
            Verdict::Failed(why) => format!("FAIL {head}: {why}"),
        }
    }
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn over(step: &Step, elapsed: Duration) -> String {
    format!(
        "took {} s, past its committed bound of {} s",
        elapsed.as_secs(),
        step.within.as_secs()
    )
}

/// Runs a qualification command.
pub trait Runner {
    /// Run `run` to completion, ending it at its deadline.
    /// # Errors
    /// [`RunError`] when it could not be started or collected, or its deadline passed.
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError>;
}

impl Runner for HostTools {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        crate::artifact::tools::Tools::run(self, run)
    }
}

/// Where a step runs.
#[derive(Clone, Debug)]
pub struct Site<'a> {
    /// Its working directory: the frozen checkout (a source step) or a scratch directory
    /// outside it (a packed step).
    pub cwd: &'a Path,
    /// The unpacked tree and its target (a packed step).
    pub packed: Option<(&'a Path, Triple)>,
    /// Where its raw stdout and stderr are kept, as `<name>[.<target>].stdout|.stderr`.
    pub logs: &'a Path,
}

fn judge(step: &Step, ran: &Ran, elapsed: Duration) -> (Option<Tests>, Verdict) {
    let tests = count_tests(&ran.text());
    let seen = (tests.summaries > 0).then_some(tests);
    let verdict = if ran.code != Some(0) {
        Verdict::Failed(match ran.code {
            Some(code) => format!("exited {code}"),
            None => "was ended by a signal".to_string(),
        })
    } else if elapsed > step.within {
        Verdict::Failed(over(step, elapsed))
    } else if tests.summaries > 0 && tests.ran() == 0 {
        Verdict::Failed("matched zero tests: a run that tested nothing is not a pass".to_string())
    } else if let Some(floor) = step.min_tests.filter(|floor| tests.passed < *floor) {
        Verdict::Failed(if tests.summaries == 0 {
            format!("printed no test result, and at least {floor} tests must pass")
        } else {
            format!(
                "{} tests passed, fewer than its floor of {floor}",
                tests.passed
            )
        })
    } else {
        Verdict::Passed
    };
    (seen, verdict)
}

/// Run one source or packed step at `site` and judge it. The raw output is written under
/// `site.logs` whatever the verdict, and its digests are in the receipt.
/// # Errors
/// A could-not-run when the output cannot be kept; a usage error for a build step, which is the
/// pack's own call ([`Receipt::build`]) or a packed step given no tree.
pub fn run_step(runner: &dyn Runner, step: &Step, site: &Site<'_>) -> Result<Receipt> {
    if step.stage == Stage::Build {
        return Err(Error::usage("the build step is the pack's own call"));
    }
    let argv: Vec<String> = match (step.stage, site.packed) {
        (Stage::Packed, Some((root, _))) => {
            let root = root.display().to_string();
            let bin = format!("{root}/bin");
            step.run
                .iter()
                .map(|w| w.replace(BIN, &bin).replace(ROOT, &root))
                .collect()
        }
        (Stage::Packed, None) => {
            return Err(Error::usage("a packed step needs the unpacked tree"));
        }
        _ => step.run.clone(),
    };
    let mut run = Run::new(&argv[0], argv[1..].iter().map(String::as_str));
    run.cwd = Some(site.cwd.to_path_buf());
    run.env_remove = super::pack::SCRUBBED_ENV
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    run.deadline = Some(step.within);
    let started = Instant::now();
    let answer = runner.run(&run);
    let elapsed = started.elapsed();
    let (ran, tests, verdict) = match answer {
        Ok(ran) => {
            let (tests, verdict) = judge(step, &ran, elapsed);
            (ran, tests, verdict)
        }
        Err(RunError::TimedOut) => (
            Ran::default(),
            None,
            Verdict::Failed(format!(
                "did not finish within its committed bound of {} s",
                step.within.as_secs()
            )),
        ),
        Err(e) => (Ran::default(), None, Verdict::Failed(e.to_string())),
    };
    let target = site.packed.map(|(_, t)| t);
    let stem = target.map_or_else(|| step.name.clone(), |t| format!("{}.{t}", step.name));
    std::fs::create_dir_all(site.logs)
        .map_err(|e| Error::io(format!("mkdir {}", site.logs.display()), &e))?;
    for (suffix, bytes) in [("stdout", &ran.stdout), ("stderr", &ran.stderr)] {
        crate::lane::report::write_atomic_bytes(
            &site.logs.join(format!("{stem}.{suffix}")),
            bytes,
        )?;
    }
    Ok(Receipt {
        name: step.name.clone(),
        stage: step.stage,
        target,
        argv,
        exit: ran.code,
        millis: millis(elapsed),
        within_secs: step.within.as_secs(),
        stdout_sha256: atpkg::tree::sha256_bytes(&ran.stdout),
        stderr_sha256: atpkg::tree::sha256_bytes(&ran.stderr),
        tests,
        verdict,
    })
}

/// Run `steps` in order at `site`, stopping at the first that does not pass (cheap-first means a
/// red formatter never waits behind a test suite). Every receipt so far is returned either way.
/// # Errors
/// As [`run_step`].
pub fn run_steps(runner: &dyn Runner, steps: &[Step], site: &Site<'_>) -> Result<Vec<Receipt>> {
    let mut receipts = Vec::new();
    for step in steps {
        let receipt = run_step(runner, step, site)?;
        let passed = receipt.passed();
        receipts.push(receipt);
        if !passed {
            break;
        }
    }
    Ok(receipts)
}
