// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spec-check`: the obligations only the Trust checkers can discharge, run on
//! purpose.
//!
//! A plain `targo test` never looks for a checker
//! ([`crate::verify::checkers_required`]), so its verdict is the same on every
//! machine. This is the other half: it requires every checker its selected
//! checks need, fails closed when one is missing, and holds the whole registry
//! to them.
//!
//! ```text
//! targo --unverified run -q -p aterm-models --bin spec-check -- [CHECK...] [--full]
//! ```
//!
//! Exit 0: every selected check passed. 1: a check failed. 2: usage. 3: could
//! not run (a checker or an input it reads is missing).
//!
//! The obligations written inside tests run on purpose too: every tiered
//! discharge (a Tier-1 `ty trace validate` beside the interpreter's) under
//! aterm-spec's `checkers` feature; [`crate::verify::checkers_required`] gives
//! the commands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::derive::{InvariantKind, Model};
use crate::xref;
use crate::{interp, verify};

mod ay;

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

/// What one check found.
enum Verdict {
    /// Passed, with a one-line account of what was held.
    Pass(String),
    /// Failed, with each finding.
    Fail(Vec<String>),
    /// An input the check reads is missing.
    CouldNotRun(String),
}

/// How a run was asked for.
struct Options {
    /// Include the minutes-class `ay` obligations.
    full: bool,
}

/// One check: what it holds, and the checker it holds it with.
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
        what: "ty agrees with the interpreter on every registered model, in each configuration \
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

    // Every checker a selected check needs, before anything runs: a missing
    // one is a run that could not happen, never a pass.
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
    // A finding is reported by the check that met it; the default hook would
    // print each caught panic a second time.
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

/// Run `f`, turning a panic into its message.
fn attempt<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_string())
    })
}

/// `work` over every item on every core, results in item order: here each
/// item's work is a checker process of its own, and in a test each is one
/// seed's walk, one fixture's sweep or one configuration's replay. A panic in
/// `work` is raised again on the caller's thread, with its own message.
pub fn each<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(items.len().max(1));
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break done };
                        done.push((i, work(item)));
                    }
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

/// `aterm-spec-models`, beside this crate.
fn spec_models() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-spec-models")
}

const LABEL: &str = "spec-check";

// ---- models ----------------------------------------------------------------

/// The findings of `work` over every registered model in every configuration
/// it declares, each named by its model.
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

/// Where `config` sits among `m`'s configurations, for a finding.
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

/// The registered model named `name`: the runs below that need a real model take a committed one
/// from the registry the binary links (`aterm_models::link_all`), not a copy of their own.
fn registered(name: &str) -> Result<Model, String> {
    xref::model_registry()
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("no model `{name}` is registered in this binary"))
}

/// A search `ty` stops short (a memory limit's stop, here forced by a 2-state
/// bound on a 7-state model) must never read as a verdict:
/// [`verify::TyCommand::output`] refuses it, and this ties that refusal to the
/// real tool's wording rather than to transcripts written here.
fn a_stopped_search_is_refused(ty: &Path) -> Option<String> {
    let m = match registered("Ring") {
        Ok(m) => m,
        Err(why) => return Some(why),
    };
    let dir = verify::scratch_dir("aterm-stop", m.name);
    let (spec, cfg) = (dir.join("Ring.tla"), dir.join("Ring.cfg"));
    let laid = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&spec, m.to_tla()))
        .and_then(|()| std::fs::write(&cfg, m.to_cfg()));
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
        "{} models, {} configuration/dial checks agree with the interpreter",
        registry.len(),
        configs.into_inner(),
    ))
}

/// One model under `ty`, in every configuration it declares: its findings,
/// the configuration/dial checks compared. An interpreter panic, including
/// exhaustion of its state budget, is a failed requested check at every dial.
fn model_under_ty(ty: &Path, all: &[Model]) -> (Vec<String>, usize) {
    let m = &all[0];
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
                // Both tiers refute a mutant: what a mutant is for. With no
                // mutant on, the model itself is broken.
                (Err((state, inv)), false) if dial == 0 => findings.push(format!(
                    "{label}: both tiers find `{inv}` violated with no mutant on, at \
                     {state:?}\n{evidence}"
                )),
                (Err(_), false) => {}
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

/// A function-valued model, which only `ty` can check: every configuration
/// holds, each design claim is broken alone by some mutant in some
/// configuration, and no `bound` is.
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

// ---- vacuity ---------------------------------------------------------------

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

/// `ty`'s dead-action report, strict (`vacuous: N dead action(s) (never
/// fired): A, B`) or relaxed (`… downgraded by --allow-vacuous=dead-action:
/// N dead action(s) (never fired): A, B`).
fn dead_actions(transcript: &str) -> Vec<String> {
    const MARKER: &str = "dead action(s) (never fired): ";
    transcript
        .lines()
        .find_map(|l| l.find(MARKER).map(|i| &l[i + MARKER.len()..]))
        .map(|names| {
            names
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// One model's strict-vacuity runs, one per configuration it declares, and
/// the interpreter's audit of them: the number of dead actions proved to be
/// negative controls.
///
/// A scalar model's dead actions are allowed (`--allow-vacuous=dead-action`)
/// only because the interpreter recomputes each configuration's dead set and
/// must agree with `ty`'s, and then proves every action dead in all of them a
/// negative control ([`verify::audit_dead_negative_controls`]); a
/// function-valued model cannot be audited, so it stays fully strict.
fn vacuity_of(ty: &Path, all: &[Model]) -> Result<usize, String> {
    let auditable = !all[0].is_function_valued();
    for config in all {
        let label = config_label(&all[0], config);
        let dir = verify::scratch_dir("aterm-vacuity", config.name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("scratch {}: {e}", dir.display()))?;
        let tla = dir.join(format!("{}.tla", config.name));
        let cfg = dir.join(format!("{}.cfg", config.name));
        std::fs::write(&tla, config.to_tla()).map_err(|e| e.to_string())?;
        std::fs::write(&cfg, config.to_cfg()).map_err(|e| e.to_string())?;
        let mut cmd = verify::TyCommand::check(ty, &tla, &cfg);
        // Exact coverage counters, so the relaxed run still names its dead set.
        cmd.arg("--strict-vacuity").arg("--coverage");
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
        if !auditable {
            continue;
        }
        // A dead set measured on a reduced space is no evidence about the model.
        verify::assert_ty_saw_whole_space(config, &text, LABEL);
        let reported = dead_actions(&text);
        let reported: Vec<&str> = reported.iter().map(String::as_str).collect();
        verify::dead_actions_agree(config, &reported).map_err(|why| format!("{label}: {why}"))?;
    }
    if !auditable {
        return Ok(0);
    }
    verify::audit_dead_negative_controls(all)
        .map_err(|why| format!("dead actions that are not audited negative controls: {why}"))
}

// ---- liveness --------------------------------------------------------------

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

// ---- trace -----------------------------------------------------------------

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
    // `Init` is open, so each trace's verdict is `Next`'s alone.
    let laid = std::fs::write(&spec, m.transition_spec()).and_then(|()| {
        std::fs::write(
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
        std::fs::write(&trace, json).map_err(|e| e.to_string())?;
        let out = verify::TyCommand::trace_validate(ty, &trace, &spec, &cfg)
            .output()
            .map_err(|e| e.to_string())?;
        Ok((out.status.success(), transcript(&out)))
    };
    let mut findings = Vec::new();
    // An honest close of a window that is not the last: the survivor is front.
    match validate([2, 1, 3, 0], [1, 2, 3, 0]) {
        Ok((true, _)) => {}
        Ok((false, text)) => findings.push(format!("a real CloseWindow step is refused\n{text}")),
        Err(e) => findings.push(e),
    }
    // The last window closes and the app does not exit: `Next` admits no such
    // step, so the refusal must come at step 1, not on the way in.
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

// ---- ay --------------------------------------------------------------------

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
mod tests {
    use super::*;

    /// An interpreter error is not a budget verdict, at the committed dial
    /// or a mutant. This malformed model fails before any external tool can
    /// run, so neither a missing checker nor checker output decides the test.
    #[test]
    fn an_interpreter_panic_is_a_failed_model_check_at_every_dial() {
        let mut model = crate::test_models::ring_model();
        model.invariants[0].expr = crate::derive::Expr::Eq(
            Box::new(crate::derive::Expr::Var("not_a_model_variable")),
            Box::new(crate::derive::Expr::Int(0)),
        );
        let dials = std::iter::once(0)
            .chain(model.mutant_dials())
            .collect::<Vec<_>>();
        let (findings, compared) = model_under_ty(
            Path::new("/this-test-must-not-run-an-external-checker"),
            &[model],
        );
        assert_eq!(compared, 0, "the model could not be interpreted");
        for dial in dials {
            assert!(
                findings.iter().any(|finding| {
                    finding.contains(&format!("Buggy={dial}"))
                        && finding.contains("not_a_model_variable")
                }),
                "the interpreter error at Buggy={dial} must fail the requested check: {findings:?}"
            );
        }
    }

    #[test]
    fn the_dead_action_report_is_read_in_both_forms() {
        assert_eq!(
            dead_actions("Warning: vacuous: 2 dead action(s) (never fired): A, B\n"),
            ["A", "B"]
        );
        assert_eq!(
            dead_actions(
                "note: vacuity warning downgraded by --allow-vacuous=dead-action: 1 dead \
                 action(s) (never fired): Slip\n"
            ),
            ["Slip"]
        );
        assert!(dead_actions("No errors found (exhaustive).\n").is_empty());
    }

    #[test]
    fn an_unknown_check_is_a_usage_error() {
        assert_eq!(main(&["no-such-check".to_string()]), 2);
        assert_eq!(main(&["--list".to_string()]), 0);
    }
}
