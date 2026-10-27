// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf restore-e2e`: a crashed window's tabs back, end to end, for N sessions. The lane
//! builds aterm's `restore_bench` bench for its arm's profile ([`bench_build::prepare`]), runs it
//! once per arm (`clean`, what ships; `copy`, each region restored from a heap copy; `lookalike`,
//! that copy with the look-alike tail planted in every region, the seam's own copy and plant off
//! the readings), and writes each run as a dated entry under `docs/measured/restore/` with its
//! provenance and the load beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::arena::{self, Halt};
use super::bench_build::{
    self, Built, Provenance, Targo, another_build, cpus, ledger_paths, load_average,
};
use super::{LaneRun, workspace_root};

/// THE BENCH'S OWN GRAMMAR for a run, mounted from the bench (std-only) as the fabric lane
/// mounts its own: the lane refuses exactly what the bench would, before the build, and no bound
/// is kept in two places.
#[path = "../../../aterm/tests/restore_bench/spec.rs"]
#[expect(
    dead_code,
    reason = "the lane validates with `parse`; the knobs a run reads back and `from_env` are the \
              bench's"
)]
mod bench_spec;

use bench_build::bench_profile::Profile;
use bench_spec::{Applied, Arm};

/// The progress lines the lane echoes from the bench.
const PROGRESS: &str = "restore bench:";

/// Parsed `verify perf restore-e2e` options.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    /// Rising session counts (`--sessions`).
    pub sessions: Option<Vec<u32>>,
    /// Crashes per session count (`--rounds`).
    pub rounds: Option<u32>,
    /// History lines per session (`--lines`).
    pub lines: Option<u32>,
    /// The arms, in order (`--arm`); empty is every arm.
    pub arms: Vec<Arm>,
    /// The look-alike arm's plant in KiB (`--lookalike-kib`).
    pub lookalike_kib: Option<u32>,
    /// Passes over the arms, forward then backward (`--passes`).
    pub passes: u32,
    pub profile: Profile,
    pub json_out: Option<PathBuf>,
}

impl Args {
    /// Parse the lane's options.
    ///
    /// # Errors
    /// A value the bench's grammar refuses, or `--json-out` for a plan of more than one run.
    pub(crate) fn from_options(options: &BTreeMap<&'static str, String>) -> Result<Args, String> {
        let mut a = Args {
            passes: 1,
            ..Args::default()
        };
        let number = |flag: &str, raw: &str| {
            raw.parse::<u32>()
                .map_err(|_| format!("`{flag}` needs a positive integer, got `{raw}`"))
        };
        for (flag, raw) in options {
            match *flag {
                "--sessions" => {
                    a.sessions = Some(
                        raw.split(',')
                            .map(|n| number(flag, n))
                            .collect::<Result<_, _>>()?,
                    );
                }
                "--rounds" => a.rounds = Some(number(flag, raw)?),
                "--lines" => a.lines = Some(number(flag, raw)?),
                "--lookalike-kib" => a.lookalike_kib = Some(number(flag, raw)?),
                "--arm" => {
                    a.arms = raw
                        .split(',')
                        .map(Arm::parse)
                        .collect::<Result<_, _>>()
                        .map_err(|_| {
                            format!(
                                "`--arm` takes clean, copy or lookalike, comma-separated, not \
                                 `{raw}`"
                            )
                        })?;
                }
                "--passes" => {
                    a.passes = number(flag, raw)?;
                    if a.passes == 0 {
                        return Err("`--passes` needs a positive integer, got `0`".to_string());
                    }
                }
                "--profile" => a.profile = Profile::parse(raw)?,
                "--json-out" => a.json_out = Some(PathBuf::from(raw)),
                "--no-windows" | "--admit" => {}
                other => {
                    return Err(format!(
                        "internal: `{other}` has no parser in the restore-e2e lane"
                    ));
                }
            }
        }
        if a.lookalike_kib.is_some() && !a.plan().contains(&Arm::Lookalike) {
            return Err("`--lookalike-kib` is the lookalike arm's: name it in `--arm`".to_string());
        }
        for arm in a.plan() {
            bench_spec::parse(&a.bench_words(Path::new("/scratch"), arm))
                .map_err(|why| format!("the restore bench refuses this run: {why}"))?;
        }
        if a.json_out.is_some() && a.plan().len() > 1 {
            return Err(
                "`--json-out` names one file, and this plan is more than one run: each run's \
                 JSON is its ledger entry's (name one `--arm`)"
                    .to_string(),
            );
        }
        Ok(a)
    }

    /// The runs, in order: the arms (every arm when none is named) forward on even passes and
    /// backward on odd ones.
    pub(crate) fn plan(&self) -> Vec<Arm> {
        let arms: Vec<Arm> = if self.arms.is_empty() {
            Arm::ALL.to_vec()
        } else {
            self.arms.clone()
        };
        (0..self.passes.max(1))
            .flat_map(|pass| {
                let mut once = arms.clone();
                if pass % 2 == 1 {
                    once.reverse();
                }
                once
            })
            .collect()
    }

    /// The bench's words for `arm`, writing to `out`.
    pub(crate) fn bench_words(&self, out: &Path, arm: Arm) -> String {
        let mut words = vec![format!("out={}", out.display())];
        if let Some(sizes) = &self.sessions {
            let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
            words.push(format!("sessions={}", sizes.join(",")));
        }
        if let Some(n) = self.rounds {
            words.push(format!("rounds={n}"));
        }
        if let Some(n) = self.lines {
            words.push(format!("lines={n}"));
        }
        words.push(format!("arm={}", arm.word()));
        if let (Arm::Lookalike, Some(kib)) = (arm, self.lookalike_kib) {
            words.push(format!("lookalike_kib={kib}"));
        }
        words.join(" ")
    }

    /// The `verify perf restore-e2e` command line that reproduces `arm`'s run alone.
    pub(crate) fn command_line(&self, arm: Arm) -> String {
        let mut line = String::from("targo --unverified verify perf restore-e2e --no-windows");
        if self.profile != Profile::Dev {
            line.push_str(&format!(" --profile {}", self.profile.name()));
        }
        if let Some(sizes) = &self.sessions {
            let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
            line.push_str(&format!(" --sessions {}", sizes.join(",")));
        }
        if let Some(n) = self.rounds {
            line.push_str(&format!(" --rounds {n}"));
        }
        if let Some(n) = self.lines {
            line.push_str(&format!(" --lines {n}"));
        }
        line.push_str(&format!(" --arm {}", arm.word()));
        if let (Arm::Lookalike, Some(kib)) = (arm, self.lookalike_kib) {
            line.push_str(&format!(" --lookalike-kib {kib}"));
        }
        line
    }

    /// Cargo's arguments for the bench and the profile.
    pub(crate) fn build_args(&self) -> Vec<&'static str> {
        vec![
            "--profile",
            self.profile.cargo_profile(),
            "-p",
            "aterm",
            "--bench",
            "restore_bench",
        ]
    }
}

/// Run the `restore-e2e` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    arena::outcome(measure(args))
}

fn measure(args: &Args) -> Result<String, Halt> {
    if !cfg!(target_os = "macos") {
        return Err(Halt::skip(
            "the crash-restore journal's live lane, which this lane measures, runs on macOS",
            "run it on a Mac",
        ));
    }
    let ledger_dir = workspace_root().join("docs/measured/restore");
    if !ledger_dir.join("README.md").is_file() {
        return Err(Halt::skip(
            format!("no restore ledger at {}", ledger_dir.display()),
            "run the lane in the development checkout, which carries docs/measured/restore",
        ));
    }
    let driver = crate::perf::driver::cargo_driver()?;
    let (targo, built, load_before) =
        bench_build::prepare(driver, args.profile, &args.build_args())?;
    let plan = args.plan();
    let mut written = Vec::with_capacity(plan.len());
    let mut load = format!("{load_before} before the build");
    for (i, arm) in plan.iter().copied().enumerate() {
        if plan.len() > 1 {
            eprintln!("  run {} of {}: arm={}", i + 1, plan.len(), arm.word());
        }
        let entry = run_arm(&targo, args, arm, &built, &ledger_dir, &load)?;
        written.push(entry.display().to_string());
        load = format!("{} before the run", load_average());
    }
    Ok(format!("ledger entries {}", written.join(", ")))
}

fn run_arm(
    targo: &Targo<'_>,
    args: &Args,
    arm: Arm,
    built: &Built,
    ledger_dir: &Path,
    load_before: &str,
) -> Result<PathBuf, Halt> {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-restore-")
        .tempdir()
        .map_err(|e| format!("could not make a scratch directory: {e}"))?;
    let words = args.bench_words(scratch.path(), arm);
    bench_build::run_bench(
        targo,
        &args.build_args(),
        bench_spec::ENV,
        &words,
        PROGRESS,
        "the restore bench",
    )?;
    let read = |name: &str| {
        let path = scratch.path().join(name);
        std::fs::read_to_string(&path)
            .map_err(|e| format!("the bench wrote no {}: {e}", path.display()))
    };
    let tables = read("restore.md")?;
    let mut report: aterm_json::Value = aterm_json::from_str(&read("restore.json")?)
        .map_err(|e| format!("the bench's restore.json does not parse: {e}"))?;
    if let Some(why) = another_build(&report, &built.path, &built.identity) {
        return Err(Halt::Fail(format!(
            "{why}: these numbers are not the `{}` arm's build, and no ledger entry was written",
            args.profile.name()
        )));
    }
    if report.get("arm").and_then(aterm_json::Value::as_str) != Some(arm.word()) {
        return Err(Halt::Fail(format!(
            "the restore bench did not report the requested arm `{}`; no ledger entry was written",
            arm.word()
        )));
    }
    validate_applied(&report, arm)
        .map_err(|why| Halt::Fail(format!("{why}; no restore ledger entry was written")))?;
    if let Some(fields) = report.as_object_mut() {
        fields.insert("lane_build".to_string(), built.lane_build.clone().into());
    }
    let json = aterm_json::to_string(&report)
        .map_err(|e| format!("the bench's report does not serialize: {e}"))?;
    let load = format!("{load_before}, {} after the run", load_average());
    eprintln!("{tables}");

    let date = super::trend::utc_date();
    let str_of = |key: &str| {
        report
            .get(key)
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    let header = Provenance {
        heading: &format!("Restore end to end: the {} arm", arm.word()),
        protocol: "docs/measured/restore/README.md",
        date: &date,
        machine: &arena::chip(),
        macos: &arena::macos_version(),
        aterm: &arena::git_provenance(&workspace_root()),
        astream: &arena::git_provenance(&workspace_root().join("vendor/astream")),
        spec: &str_of("spec"),
        profile: &str_of("profile"),
        elapsed_s: report
            .get("elapsed_s")
            .and_then(aterm_json::Value::as_f64)
            .unwrap_or(f64::NAN),
        command: &args.command_line(arm),
        build: &built.lane_build,
        thermal: &arena::thermal(),
        cpus: &cpus(),
        load: &load,
    }
    .render();
    let (md_path, json_path) = ledger_paths(ledger_dir, &date, "restore-e2e");
    std::fs::write(&md_path, format!("{header}\n{tables}\n"))
        .map_err(|e| format!("could not write {}: {e}", md_path.display()))?;
    std::fs::write(&json_path, &json)
        .map_err(|e| format!("could not write {}: {e}", json_path.display()))?;
    eprintln!(
        "ledger entry written: {} (+ {}) — commit both to record the run",
        md_path.display(),
        json_path.display()
    );
    if let Some(out) = &args.json_out {
        std::fs::write(out, &json)
            .map_err(|e| format!("could not write {}: {e}", out.display()))?;
        eprintln!("json written: {}", out.display());
    }
    Ok(md_path)
}

/// Every raw round must carry the requested arm's observed operations, rather than its label
/// alone. Old reports without observations cannot establish that an experimental arm ran.
fn validate_applied(report: &aterm_json::Value, arm: Arm) -> Result<(), String> {
    use aterm_json::Value;
    let sizes = report
        .get("sizes")
        .and_then(Value::as_array)
        .filter(|sizes| !sizes.is_empty())
        .ok_or("the restore report has no measured session counts")?;
    for size in sizes {
        let sessions = size
            .get("sessions")
            .and_then(Value::as_u64)
            .filter(|sessions| *sessions > 0)
            .ok_or("the restore report has no positive session count")?;
        let rounds = size
            .get("rounds")
            .and_then(Value::as_array)
            .filter(|rounds| !rounds.is_empty())
            .ok_or("the restore report has no observed rounds")?;
        for round in rounds {
            if round.get("sessions").and_then(Value::as_u64) != Some(sessions) {
                return Err("the restore round does not match its session count".into());
            }
            let observed = round
                .get("experiment")
                .ok_or("the restore round has no observed experiment")?;
            let count = |field| {
                observed
                    .get(field)
                    .and_then(Value::as_u64)
                    .ok_or_else(|| format!("the restore round has no observed {field} count"))
            };
            let applied = Applied {
                copies: count("copies")?,
                plants: count("plants")?,
                refused: count("refused")?,
            };
            if !applied.matches(arm, sessions) {
                return Err(format!(
                    "the restore round did not apply {:?} to all {sessions} sessions: {applied:?}",
                    arm
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(pairs: &[(&'static str, &str)]) -> BTreeMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
    }

    #[test]
    fn ledger_acceptance_requires_observed_application_not_the_requested_arm() {
        let report = |applied: Applied| {
            aterm_json::json!({
                "sizes": [{"sessions": 2, "rounds": [{
                    "sessions": 2,
                    "experiment": {"copies": applied.copies, "plants": applied.plants,
                                   "refused": applied.refused}
                }]}]
            })
        };
        let copy = Applied {
            copies: 2,
            plants: 0,
            refused: 0,
        };
        let planted = Applied { plants: 2, ..copy };
        for (arm, applied) in [
            (Arm::Clean, Applied::default()),
            (Arm::Copy, copy),
            (Arm::Lookalike, planted),
        ] {
            assert!(validate_applied(&report(applied), arm).is_ok());
        }
        for applied in [
            Applied::default(),
            copy,
            Applied {
                plants: 1,
                ..planted
            },
            Applied {
                refused: 1,
                ..planted
            },
        ] {
            assert!(validate_applied(&report(applied), Arm::Lookalike).is_err());
        }
        assert!(validate_applied(&aterm_json::json!({}), Arm::Clean).is_err());
        assert!(validate_applied(&aterm_json::json!({"sizes": []}), Arm::Clean).is_err());
        assert!(
            validate_applied(
                &aterm_json::json!({"sizes": [{"sessions": 2, "rounds": [{"sessions": 2}]}]}),
                Arm::Copy
            )
            .is_err()
        );
    }

    #[test]
    fn the_default_plan_is_every_arm_with_the_bench_s_defaults() {
        let a = Args::from_options(&options(&[("--no-windows", "")])).expect("parses");
        assert_eq!(a.plan(), Arm::ALL);
        assert_eq!(
            a.bench_words(Path::new("/s"), Arm::Clean),
            "out=/s arm=clean"
        );
        assert_eq!(
            a.command_line(Arm::Lookalike),
            "targo --unverified verify perf restore-e2e --no-windows --arm lookalike"
        );
        assert_eq!(
            a.build_args(),
            [
                "--profile",
                "test",
                "-p",
                "aterm",
                "--bench",
                "restore_bench"
            ]
        );
        let spec = bench_spec::parse(&a.bench_words(Path::new("/s"), Arm::Lookalike))
            .expect("the bench reads the lane's words");
        assert_eq!((spec.sessions, spec.arm), (vec![1, 8, 32], Arm::Lookalike));
    }

    #[test]
    fn every_knob_reaches_the_bench_and_the_entry_s_command_line() {
        let a = Args::from_options(&options(&[
            ("--sessions", "2,16"),
            ("--rounds", "5"),
            ("--lines", "800"),
            ("--arm", "lookalike,clean"),
            ("--lookalike-kib", "64"),
            ("--passes", "2"),
            ("--profile", "opt"),
        ]))
        .expect("parses");
        assert_eq!(
            a.plan(),
            [Arm::Lookalike, Arm::Clean, Arm::Clean, Arm::Lookalike],
            "A B B A"
        );
        assert_eq!(
            a.bench_words(Path::new("/s"), Arm::Lookalike),
            "out=/s sessions=2,16 rounds=5 lines=800 arm=lookalike lookalike_kib=64"
        );
        assert_eq!(
            a.bench_words(Path::new("/s"), Arm::Clean),
            "out=/s sessions=2,16 rounds=5 lines=800 arm=clean",
            "the plant is the lookalike arm's alone"
        );
        assert_eq!(
            a.command_line(Arm::Lookalike),
            "targo --unverified verify perf restore-e2e --no-windows --profile opt --sessions \
             2,16 --rounds 5 --lines 800 --arm lookalike --lookalike-kib 64"
        );
        assert_eq!(a.build_args()[1], "opt-test");
    }

    #[test]
    fn what_the_bench_would_refuse_is_refused_before_the_build() {
        for pairs in [
            &[("--sessions", "8,1")][..],
            &[("--sessions", "0")],
            &[("--sessions", "65")],
            &[("--rounds", "0")],
            &[("--lines", "x")],
            &[("--arm", "dirty")],
            &[("--arm", "clean,,copy")],
            &[("--lookalike-kib", "64"), ("--arm", "clean")],
            &[("--lookalike-kib", "0")],
            &[("--passes", "0")],
            &[("--profile", "release")],
            &[("--json-out", "/tmp/r.json")],
        ] {
            assert!(Args::from_options(&options(pairs)).is_err(), "{pairs:?}");
        }
        assert!(
            Args::from_options(&options(&[
                ("--json-out", "/tmp/r.json"),
                ("--arm", "copy")
            ]))
            .is_ok(),
            "one arm, one run, one file"
        );
    }
}
