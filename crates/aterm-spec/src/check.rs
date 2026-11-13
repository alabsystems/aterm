// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spec-check`: model law audits and the obligations the Trust checkers discharge.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crate::derive::{InvariantKind, Model};
use crate::xref;
use crate::{interp, verify};

mod ay;

/// Run a worker's queue, or decline an auxiliary worker before it claims any item. Worker zero
/// must run exactly once: it inherits the calling job, so it keeps draining the queue while
/// auxiliaries wait for admission. Nested queues inherit their caller's job in the same way.
/// The stop predicate means the queue is exhausted or another worker panicked.
pub type WorkerExecutor = dyn Fn(usize, &dyn Fn() -> bool, &mut dyn FnMut()) + Send + Sync;

thread_local! {
    static WORKER_EXECUTOR: std::cell::RefCell<Option<Arc<WorkerExecutor>>> = const { std::cell::RefCell::new(None) };
}

/// Install worker admission for this call and its nested queues, restoring the caller on exit.
/// # Panics
/// The work or its installed admission executor panics.
pub fn with_worker_executor<R>(executor: Arc<WorkerExecutor>, work: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<WorkerExecutor>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WORKER_EXECUTOR.with_borrow_mut(|current| *current = self.0.take());
        }
    }
    let _restore = Restore(WORKER_EXECUTOR.replace(Some(executor)));
    work()
}

/// A checker binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Ty,
    Ay,
}

impl Tool {
    const fn name(self) -> &'static str {
        match self {
            Tool::Ty => "ty",
            Tool::Ay => "ay",
        }
    }

    fn locate(self) -> Option<verify::Located> {
        match self {
            Tool::Ty => verify::locate_ty(),
            Tool::Ay => verify::locate_ay(),
        }
    }
}

enum Verdict {
    /// Passed, with a one-line account of what was held.
    Pass(String),
    /// Failed, with each finding.
    Fail(Vec<String>),
    /// An input the check reads is missing.
    CouldNotRun(String),
}

struct Options {
    /// Include the minutes-class `ay` obligations.
    full: bool,
}

struct Check {
    name: &'static str,
    needs: Tool,
    what: &'static str,
    run: fn(&Path, &Options) -> Verdict,
}

const CHECKS: &[Check] = &[
    Check {
        name: "models",
        needs: Tool::Ty,
        what: "declared bounds are ranges no mutant breaks, and each design claim has a mutant; \
               ty agrees with the interpreter on every registered model, in each configuration \
               it declares and at each mutant dial, over the same reachable space; a \
               function-valued model holds and has each design claim broken by a mutant; a \
               search ty stops short is never a verdict",
        run: models,
    },
    Check {
        name: "vacuity",
        needs: Tool::Ty,
        what: "`ty check --strict-vacuity` passes every registered model; a dead action is \
               allowed only as an audited negative control",
        run: vacuity,
    },
    Check {
        name: "liveness",
        needs: Tool::Ty,
        what: "every registered liveness obligation holds under ty over the interpreter's space, \
               and each named mutant breaks it",
        run: liveness,
    },
    Check {
        name: "trace",
        needs: Tool::Ty,
        what: "`ty trace validate` accepts a real WindowRouting step and refuses a corrupted one",
        run: trace,
    },
    Check {
        name: "ay",
        needs: Tool::Ay,
        what: "every obligation under aterm-spec-models/proofs/ay gets the verdict it declares \
               (`set-info :status`), all from one attributed solver build",
        run: ay_bundles,
    },
];

const USAGE: &str = "\
spec-check — the obligations only the Trust checkers (ty, ay) discharge

usage: spec-check [CHECK...] [--full] [--list]

  CHECK    one or more of the checks `--list` names (default: all of them)
  --full   also solve the minutes-class ay obligations
  --list   name every check, what it holds and the checker it needs

exit: 0 passed · 1 a check failed · 2 usage · 3 could not run (a checker or input missing)";

/// The whole CLI: parse `args` (without the program name), run the selected
/// checks, print what each found, and return the exit code.
// Skip: the command-line driver (printing, process work). Spec tooling.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let mut options = Options { full: false };
    let mut selected: Vec<&Check> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--list" => {
                for check in CHECKS {
                    println!("{:<9} [{}] {}", check.name, check.needs.name(), check.what);
                }
                return 0;
            }
            "--full" => options.full = true,
            name => match CHECKS.iter().find(|check| check.name == name) {
                Some(check) if !selected.iter().any(|s| s.name == name) => selected.push(check),
                Some(_) => {}
                None => {
                    eprintln!("spec-check: no check named `{name}`\n\n{USAGE}");
                    return 2;
                }
            },
        }
    }
    if selected.is_empty() {
        selected = CHECKS.iter().collect();
    }

    let mut tools: BTreeMap<&'static str, PathBuf> = BTreeMap::new();
    for check in &selected {
        let tool = check.needs;
        if tools.contains_key(tool.name()) {
            continue;
        }
        match tool.locate() {
            Some(found) => {
                tools.insert(tool.name(), found.path);
            }
            None => {
                println!(
                    "spec-check: could not run `{}`: it needs Trust `{}`, which was not found. \
                     Install it: aterm pkg install {}",
                    check.name,
                    tool.name(),
                    tool.name()
                );
                return 3;
            }
        }
    }
    verify::require_checkers();
    std::panic::set_hook(Box::new(|_| {}));

    let started = Instant::now();
    println!(
        "spec-check: {} · {}",
        selected
            .iter()
            .map(|check| check.name)
            .collect::<Vec<_>>()
            .join(" "),
        tools
            .iter()
            .map(|(name, path)| format!("{name} = {}", path.display()))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    let (mut failed, mut not_run) = (Vec::new(), Vec::new());
    for check in &selected {
        let t0 = Instant::now();
        let tool = &tools[check.needs.name()];
        let verdict = attempt(|| (check.run)(tool, &options))
            .unwrap_or_else(|why| Verdict::Fail(vec![format!("the check itself failed: {why}")]));
        let secs = t0.elapsed().as_secs_f64();
        match verdict {
            Verdict::Pass(detail) => println!("ok    {:<9} {secs:>6.1}s  {detail}", check.name),
            Verdict::Fail(findings) => {
                println!(
                    "FAIL  {:<9} {secs:>6.1}s  {} finding(s)",
                    check.name,
                    findings.len()
                );
                failed.push((check.name, findings));
            }
            Verdict::CouldNotRun(why) => {
                println!("skip  {:<9} {secs:>6.1}s  could not run: {why}", check.name);
                not_run.push(check.name);
            }
        }
    }
    for (name, findings) in &failed {
        println!("\n{name}:");
        for finding in findings {
            for (i, line) in finding.lines().enumerate() {
                println!("{}{line}", if i == 0 { "  - " } else { "    " });
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    if !failed.is_empty() {
        println!(
            "\nspec-check: FAILED ({} of {} checks) in {secs:.1}s; rerun one: spec-check {}",
            failed.len(),
            selected.len(),
            failed[0].0
        );
        1
    } else if !not_run.is_empty() {
        println!(
            "\nspec-check: COULD NOT RUN {} in {secs:.1}s",
            not_run.join(" ")
        );
        3
    } else {
        println!("\nspec-check: ok ({} checks) in {secs:.1}s", selected.len());
        0
    }
}

/// `f`'s result, or its panic's message: for a fan-out whose every item must run and report
/// whatever another item does ([`each`] re-raises the first panic, and a worker that meets one
/// runs no further items).
pub fn attempt<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_string())
    })
}

/// `work` over every item on every core, results in item order.
pub fn each<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let cores = aterm_cores::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    each_on(cores, items, work)
}

/// [`each`] on at most `workers` threads: for items whose work is mostly file system calls,
/// which the kernel serves no faster past a few callers at once.
pub fn each_on<T: Sync, R: Send>(
    workers: usize,
    items: &[T],
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    each_after(workers, |_| {}, items, work)
}

/// [`each_on`], each worker running `first` before it claims an item; what `first` is given says
/// whether every item has been claimed already.
pub(crate) fn each_after<T: Sync, R: Send>(
    workers: usize,
    first: impl Fn(&dyn Fn() -> bool) + Sync,
    items: &[T],
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let panicked = AtomicBool::new(false);
    let workers = workers.clamp(1, items.len().max(1));
    let executor = WORKER_EXECUTOR.with_borrow(Clone::clone);
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let executor = executor.clone();
                let (next, first, work, panicked) = (&next, &first, &work, &panicked);
                scope.spawn(move || {
                    struct PanicEnd<'a>(&'a AtomicBool);
                    impl Drop for PanicEnd<'_> {
                        fn drop(&mut self) {
                            if std::thread::panicking() {
                                self.0.store(true, Ordering::SeqCst);
                            }
                        }
                    }
                    let _panic_end = PanicEnd(panicked);
                    let mut done = Vec::new();
                    let spent = || next.load(Ordering::Relaxed) >= items.len();
                    let stop_admission = || spent() || panicked.load(Ordering::SeqCst);
                    let mut run = || {
                        first(&spent);
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(i) else { break };
                            done.push((i, work(item)));
                        }
                    };
                    match executor {
                        Some(executor) => with_worker_executor(executor.clone(), || {
                            executor(worker, &stop_admission, &mut run);
                        }),
                        None => run(),
                    }
                    done
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| {
                h.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
#[path = "check_worker_executor_tests.rs"]
mod worker_executor_tests;

/// aterm-spec-models, beside the running package under `crates/`: spec-check runs through
/// `targo run -p aterm-models` (verify's spec row and every bundle's README), which names that
/// package's directory when it starts the binary ([`aterm_tempfile::manifest_dir`]).
fn spec_models() -> PathBuf {
    aterm_tempfile::manifest_dir().join("../aterm-spec-models")
}

const LABEL: &str = "spec-check";

fn over_configurations(
    models: &[Vec<Model>],
    work: impl Fn(&[Model]) -> Vec<String> + Sync,
) -> Vec<String> {
    each(models, |configs| {
        attempt(|| work(configs)).unwrap_or_else(|why| vec![format!("{}: {why}", configs[0].name)])
    })
    .into_iter()
    .flatten()
    .collect()
}

fn config_label(m: &Model, config: &Model) -> String {
    let overrides: Vec<String> = config
        .consts
        .iter()
        .filter(|c| !m.consts.contains(c))
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    if overrides.is_empty() {
        m.name.to_string()
    } else {
        format!("{} [scenario {}]", m.name, overrides.join(" "))
    }
}

fn registered(name: &str) -> Result<Model, String> {
    xref::model_registry()
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("no model `{name}` is registered in this binary"))
}

fn a_stopped_search_is_refused(ty: &Path) -> Option<String> {
    let m = match registered("Ring") {
        Ok(m) => m,
        Err(why) => return Some(why),
    };
    let dir = verify::scratch_dir("aterm-stop", m.name);
    let (spec, cfg) = (dir.join("Ring.tla"), dir.join("Ring.cfg"));
    let laid = std::fs::create_dir_all(&dir)
        .and_then(|()| verify::write_scratch(&spec, m.to_tla()))
        .and_then(|()| verify::write_scratch(&cfg, m.to_cfg()));
    if let Err(e) = laid {
        return Some(format!("scratch {}: {e}", dir.display()));
    }
    let run = verify::TyCommand::check(ty, &spec, &cfg)
        .arg("--max-states=2")
        .output();
    let _ = std::fs::remove_dir_all(&dir);
    match run {
        Err(e) if e.to_string().contains("INCOMPLETE") => None,
        Err(e) => Some(format!("a ty run under a 2-state bound failed: {e}")),
        Ok(out) => Some(format!(
            "ty stopped by a 2-state bound reads as a verdict: the stop classifier no longer \
             matches ty's wording\n{}",
            transcript(&out)
        )),
    }
}

fn models(ty: &Path, _: &Options) -> Verdict {
    if let Some(finding) = a_stopped_search_is_refused(ty) {
        return Verdict::Fail(vec![finding]);
    }
    let registry = xref::model_configurations();
    let configs = AtomicUsize::new(0);
    let findings = over_configurations(&registry, |all| {
        let (findings, held) = model_under_ty(ty, all);
        configs.fetch_add(held, Ordering::Relaxed);
        findings
    });
    if !findings.is_empty() {
        return Verdict::Fail(findings);
    }
    Verdict::Pass(format!(
        "{} models pass the law audit; {} configuration/dial checks agree with the interpreter",
        registry.len(),
        configs.into_inner(),
    ))
}

fn model_under_ty(ty: &Path, all: &[Model]) -> (Vec<String>, usize) {
    let m = &all[0];
    // Agreement on the first failed law cannot establish that a later law is a bound or
    // that every claim has a witness. Walk all laws before accepting either checker's verdict.
    let findings = attempt(|| verify::sweep_faults(all)).unwrap_or_else(|why| {
        vec![format!(
            "{}: the model law audit could not run: {why}",
            m.name
        )]
    });
    if !findings.is_empty() {
        return (findings, 0);
    }
    if m.is_function_valued() {
        return function_valued_under_ty(ty, all);
    }
    let has_dial = m.consts.iter().any(|(name, _)| *name == "Buggy");
    let mut dials = vec![0];
    if has_dial {
        dials.extend(m.mutant_dials());
    }
    let (mut findings, mut compared) = (Vec::new(), 0);
    for config in all {
        let label = config_label(m, config);
        for &dial in &dials {
            let at = interp::with_buggy(config, dial);
            let interpreted = match attempt(|| interp::bmc(&at)) {
                Ok(interpreted) => interpreted,
                Err(why) => {
                    findings.push(format!(
                        "{label} at Buggy={dial}: the interpreter could not check the model: {why}"
                    ));
                    continue;
                }
            };
            let (held, evidence) = verify::ty_check_derived(ty, &at, &at.to_cfg(), LABEL);
            compared += 1;
            match (interpreted, held) {
                (Ok(states), true) => {
                    let explored = verify::ty_states_explored(&evidence);
                    if explored != Some(states as u64) {
                        findings.push(format!(
                            "{label} at Buggy={dial}: ty explored {explored:?} states, the \
                             interpreter {states}: one tier's proof is about a space the other \
                             never saw\n{evidence}"
                        ));
                    }
                }
                (Err((state, inv)), false) if dial == 0 => findings.push(format!(
                    "{label}: both tiers find `{inv}` violated with no mutant on, at \
                     {state:?}\n{evidence}"
                )),
                (Err((state, inv)), false) => {
                    let named = verify::ty_violated_law(&evidence);
                    if !at.invariants.iter().any(|law| Some(law.name) == named) {
                        findings.push(format!(
                            "{label} at Buggy={dial}: ty failed without refuting a law, where \
                             the interpreter refutes `{inv}` at {state:?}\n{evidence}"
                        ));
                    }
                }
                (Ok(states), false) => findings.push(format!(
                    "{label} at Buggy={dial}: ty refutes what the interpreter proves over \
                     {states} states\n{evidence}"
                )),
                (Err((state, inv)), true) => findings.push(format!(
                    "{label} at Buggy={dial}: ty proves what the interpreter refutes (`{inv}` \
                     at {state:?}) — a false proof\n{evidence}"
                )),
            }
        }
    }
    (findings, compared)
}

fn function_valued_under_ty(ty: &Path, all: &[Model]) -> (Vec<String>, usize) {
    let m = &all[0];
    let mut findings = Vec::new();
    for config in all {
        let at = interp::with_buggy(config, 0);
        let (ok, evidence) = verify::ty_check_derived(ty, &at, &at.to_cfg(), LABEL);
        if !ok {
            findings.push(format!(
                "{}: ty refutes it with no mutant on\n{evidence}",
                config_label(m, config)
            ));
        }
    }
    let dials = m.mutant_dials();
    for inv in &m.invariants {
        let caught = all.iter().any(|config| {
            let mut alone = config.clone();
            alone.invariants.retain(|other| other.name == inv.name);
            dials.iter().any(|&dial| {
                let at = interp::with_buggy(&alone, dial);
                !verify::ty_check_derived(ty, &at, &at.to_cfg(), LABEL).0
            })
        });
        match (inv.kind, caught) {
            (InvariantKind::Claim, false) => findings.push(format!(
                "{}::{}: no mutant breaks it under ty — give it one, or delete it",
                m.name, inv.name
            )),
            (InvariantKind::Bound, true) => findings.push(format!(
                "{}::{}: a mutant breaks it under ty — it is a caught design claim: declare it \
                 `invariant`",
                m.name, inv.name
            )),
            _ => {}
        }
    }
    (findings, all.len())
}

fn vacuity(ty: &Path, _: &Options) -> Verdict {
    let registry = xref::model_configurations();
    let dead = AtomicUsize::new(0);
    let with_dead = AtomicUsize::new(0);
    let findings = over_configurations(&registry, |all| match vacuity_of(ty, all) {
        Ok(0) => Vec::new(),
        Ok(n) => {
            dead.fetch_add(n, Ordering::Relaxed);
            with_dead.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }
        Err(why) => vec![format!("{}: {why}", all[0].name)],
    });
    if !findings.is_empty() {
        return Verdict::Fail(findings);
    }
    Verdict::Pass(format!(
        "{} models pass `ty check --strict-vacuity` in every configuration; {} dead actions in \
         {} models are audited negative controls",
        registry.len(),
        dead.into_inner(),
        with_dead.into_inner()
    ))
}

struct TyCoverage {
    states: u64,
    actions: Vec<(String, u64)>,
}

fn action_coverage(bytes: &[u8]) -> Result<TyCoverage, String> {
    let report: aterm_json::Value =
        aterm_json::from_slice(bytes).map_err(|e| format!("invalid ty coverage JSON: {e}"))?;
    if report.get("tool").and_then(aterm_json::Value::as_str) != Some("ty") {
        return Err("coverage JSON does not identify ty".into());
    }
    let capabilities = report
        .get("capabilities")
        .and_then(aterm_json::Value::as_array)
        .ok_or("ty coverage JSON has no capabilities")?;
    if capabilities.iter().any(|value| value.as_str().is_none()) {
        return Err("ty coverage JSON has malformed capabilities".into());
    }
    for required in ["check-json-contract-v2", "check-exact-source-v2"] {
        if !capabilities.iter().any(|v| v.as_str() == Some(required)) {
            return Err(format!("ty coverage JSON lacks {required}"));
        }
    }
    if report
        .get("completeness")
        .and_then(aterm_json::Value::as_str)
        != Some("exhaustive")
        || report
            .get("result")
            .and_then(|v| v.get("status"))
            .and_then(aterm_json::Value::as_str)
            != Some("ok")
    {
        return Err("ty coverage JSON is not an exhaustive clean verdict".into());
    }
    let statistics = report
        .get("statistics")
        .ok_or("ty coverage JSON has no statistics")?;
    let states = statistics
        .get("states_found")
        .and_then(aterm_json::Value::as_u64)
        .filter(|states| *states > 0)
        .ok_or("ty coverage JSON has no positive state count")?;
    if statistics
        .get("suppressed_guard_errors")
        .and_then(aterm_json::Value::as_u64)
        != Some(0)
    {
        return Err("ty coverage JSON does not establish zero suppressed guard errors".into());
    }
    let mut actions = Vec::new();
    if let Some(rows) = report.get("actions_detected") {
        for row in rows
            .as_array()
            .ok_or("ty action coverage is not an array")?
        {
            let name = row
                .get("name")
                .and_then(aterm_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or("ty action coverage has no action name")?;
            let occurrences = row
                .get("occurrences")
                .and_then(aterm_json::Value::as_u64)
                .ok_or("ty action coverage has no occurrence count")?;
            if actions.iter().any(|(seen, _)| seen == name) {
                return Err(format!("ty action coverage repeats {name}"));
            }
            actions.push((name.to_owned(), occurrences));
        }
    }
    Ok(TyCoverage { states, actions })
}

fn vacuity_of(ty: &Path, all: &[Model]) -> Result<usize, String> {
    let auditable = !all[0].is_function_valued();
    for config in all {
        let label = config_label(&all[0], config);
        let dir = verify::scratch_dir("aterm-vacuity", config.name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("scratch {}: {e}", dir.display()))?;
        let tla = dir.join(format!("{}.tla", config.name));
        let cfg = dir.join(format!("{}.cfg", config.name));
        verify::write_scratch(&tla, config.to_tla()).map_err(|e| e.to_string())?;
        verify::write_scratch(&cfg, config.to_cfg()).map_err(|e| e.to_string())?;
        let mut cmd = verify::TyCommand::check(ty, &tla, &cfg);
        cmd.arg("--strict-vacuity")
            .arg("--coverage")
            .arg("--output")
            .arg("json")
            .arg("--no-cache")
            .arg("--no-gpu");
        if auditable {
            cmd.arg("--allow-vacuous=dead-action");
        }
        let out = cmd.output();
        let _ = std::fs::remove_dir_all(&dir);
        let out = out.map_err(|e| e.to_string())?;
        let text = format!(
            "{}{}\n{}",
            verify::ty_evidence_header(ty),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            return Err(format!(
                "{label}: `ty check --strict-vacuity` fails (vacuous, or a violation){}\n{text}",
                if auditable {
                    ""
                } else {
                    " — function-valued, so no dead action is allowed"
                }
            ));
        }
        let coverage =
            action_coverage(&out.stdout).map_err(|why| format!("{label}: {why}\n{text}"))?;
        if coverage.actions.is_empty() && !config.actions.is_empty() {
            return Err(format!("{label}: ty reported no action coverage\n{text}"));
        }
        let reported: Vec<&str> = coverage
            .actions
            .iter()
            .filter(|(_, count)| *count == 0)
            .map(|(name, _)| name.as_str())
            .collect();
        if !auditable {
            if !reported.is_empty() {
                return Err(format!(
                    "{label}: strict ty verdict retained dead actions\n{text}"
                ));
            }
            continue;
        }
        verify::assert_ty_saw_whole_space_count(config, coverage.states, &text, LABEL);
        verify::dead_actions_agree(config, &reported).map_err(|why| format!("{label}: {why}"))?;
    }
    if !auditable {
        return Ok(0);
    }
    verify::audit_dead_negative_controls(all)
        .map_err(|why| format!("dead actions that are not audited negative controls: {why}"))
}

fn liveness(ty: &Path, _: &Options) -> Verdict {
    let obligations = xref::liveness_registry();
    let findings: Vec<String> = each(&obligations, |(m, live)| {
        attempt(|| liveness_under_ty(ty, m, live))
            .unwrap_or_else(|why| vec![format!("{} `{}`: {why}", m.name, live.name)])
    })
    .into_iter()
    .flatten()
    .collect();
    if !findings.is_empty() {
        return Verdict::Fail(findings);
    }
    Verdict::Pass(format!(
        "{} obligations hold under ty over the interpreter's space and break under each mutant",
        obligations.len()
    ))
}

fn liveness_under_ty(ty: &Path, m: &Model, live: &crate::derive::Liveness) -> Vec<String> {
    let mut findings = Vec::new();
    let committed = interp::with_buggy(m, 0);
    let space = interp::Space::of(&committed);
    let (held, evidence) = verify::ty_check_liveness(
        ty,
        m,
        &live.to_tla(&committed),
        &live.to_cfg_with(m, &[("Buggy", 0)]),
        "committed",
        LABEL,
    );
    if !held {
        findings.push(format!(
            "{} `{}`: ty refutes it at the committed config\n{evidence}",
            m.name, live.name
        ));
    } else if verify::ty_states_explored(&evidence) != Some(space.states() as u64) {
        findings.push(format!(
            "{} `{}`: ty explored {:?} states, the interpreter {}\n{evidence}",
            m.name,
            live.name,
            verify::ty_states_explored(&evidence),
            space.states()
        ));
    }
    for mutant in &live.mutants {
        let isolated = verify::isolate_mutant(m, space.fired(), mutant);
        let (held, evidence) = verify::ty_check_liveness(
            ty,
            m,
            &live.to_tla(&isolated),
            &live.to_cfg_with(m, &[("Buggy", 1)]),
            mutant,
            LABEL,
        );
        if held {
            findings.push(format!(
                "{} `{}`: ty finds no violation with mutant `{mutant}` alone\n{evidence}",
                m.name, live.name
            ));
        } else if !(evidence.contains("Temporal properties were violated")
            || evidence.contains("Liveness violation"))
        {
            findings.push(format!(
                "{} `{}`: ty rejects mutant `{mutant}`, but not with a temporal violation\n\
                 {evidence}",
                m.name, live.name
            ));
        }
    }
    findings
}

fn transcript(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn trace(ty: &Path, _: &Options) -> Verdict {
    let m = match registered("WindowRouting") {
        Ok(m) => m,
        Err(why) => return Verdict::Fail(vec![why]),
    };
    let dir = verify::scratch_dir("aterm-trace", m.name);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Verdict::CouldNotRun(format!("scratch {}: {e}", dir.display()));
    }
    let spec = dir.join(format!("{}.tla", m.name));
    let cfg = dir.join(format!("{}.cfg", m.name));
    let trace = dir.join("t.json");
    let laid = verify::write_scratch(&spec, m.transition_spec()).and_then(|()| {
        verify::write_scratch(
            &cfg,
            m.transition_cfg(&[("MaxWin", 1_000_000), ("MaxId", 1_000_000_000)]),
        )
    });
    if let Err(e) = laid {
        return Verdict::CouldNotRun(e.to_string());
    }
    let state = |[win_count, frontmost, next_id, exited]: [i64; 4]| {
        BTreeMap::from([
            ("win_count", win_count),
            ("frontmost", frontmost),
            ("next_id", next_id),
            ("exited", exited),
        ])
    };
    let validate = |prev: [i64; 4], next: [i64; 4]| -> Result<(bool, String), String> {
        let json =
            verify::transition_trace_json(&m, &state(prev), &state(next), Some("CloseWindow"));
        verify::write_scratch(&trace, json).map_err(|e| e.to_string())?;
        let out = verify::TyCommand::trace_validate(ty, &trace, &spec, &cfg)
            .output()
            .map_err(|e| e.to_string())?;
        Ok((out.status.success(), transcript(&out)))
    };
    let mut findings = Vec::new();
    match validate([2, 1, 3, 0], [1, 2, 3, 0]) {
        Ok((true, _)) => {}
        Ok((false, text)) => findings.push(format!("a real CloseWindow step is refused\n{text}")),
        Err(e) => findings.push(e),
    }
    match validate([1, 1, 2, 0], [0, 0, 2, 0]) {
        Ok((false, text)) if text.contains("trace step 1 has no matching spec states") => {}
        Ok((ok, text)) => findings.push(format!(
            "a CloseWindow that empties the window set without exiting is {} — not refused \
             at step 1\n{text}",
            if ok { "accepted" } else { "refused" }
        )),
        Err(e) => findings.push(e),
    }
    let _ = std::fs::remove_dir_all(&dir);
    if findings.is_empty() {
        Verdict::Pass("a real step conforms and a corrupted one is refused at step 1".to_string())
    } else {
        Verdict::Fail(findings)
    }
}

fn ay_bundles(ay_bin: &Path, options: &Options) -> Verdict {
    let root = spec_models().join("proofs/ay");
    if !root.is_dir() {
        return Verdict::CouldNotRun(format!("no proof bundles at {}", root.display()));
    }
    let (found, mut findings) = ay::obligations(&root);
    let (rows, deferred): (Vec<ay::Obligation>, Vec<ay::Obligation>) = found
        .into_iter()
        .partition(|obligation| options.full || !obligation.minutes);
    let answers = each(&rows, |obligation| ay::solve(ay_bin, obligation));
    let mut builds = std::collections::BTreeSet::new();
    let mut bundles = std::collections::BTreeSet::new();
    for (obligation, answer) in rows.iter().zip(answers) {
        let name = obligation
            .path
            .strip_prefix(&root)
            .unwrap_or(&obligation.path)
            .display()
            .to_string();
        bundles.insert(obligation.path.parent().map(Path::to_path_buf));
        match answer {
            Ok(answer) => {
                if let Some(build) = &answer.stamp {
                    builds.insert(build.clone());
                }
                if let Err(why) = ay::judge(&name, obligation.want, &answer) {
                    findings.push(why);
                }
            }
            Err(why) => findings.push(format!("{name}: {why}")),
        }
    }
    if builds.len() > 1 {
        findings.push(format!(
            "the verdicts came from more than one solver build, so the run is no one \
             discharge: {builds:?}"
        ));
    }
    if !findings.is_empty() {
        return Verdict::Fail(findings);
    }
    Verdict::Pass(format!(
        "{} obligations in {} bundles, each as declared, from ay {}{}",
        rows.len(),
        bundles.len(),
        builds.into_iter().next().unwrap_or_default(),
        if deferred.is_empty() {
            String::new()
        } else {
            format!("; {} minutes-class left to --full", deferred.len())
        }
    ))
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
