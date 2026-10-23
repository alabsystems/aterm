// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf fabric` benchmark lane runner and provenance ledger writer.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio};

use super::arena::{self, Halt};
use super::{LaneRun, workspace_root};
use crate::perf::driver::CargoDriver;

/// THE BENCH'S OWN GRAMMAR for a run, mounted from the bench (std-only) the way
#[path = "../../../aterm/tests/fabric_bench/spec.rs"]
#[expect(
    dead_code,
    reason = "the lane validates with `parse`; the knobs a run reads back and `from_env` are the \
              bench's"
)]
mod bench_spec;

/// THE BENCH'S OWN TABLE of arms, mounted the same way: the cargo profile each
#[path = "../../../aterm/tests/fabric_bench/profile.rs"]
mod bench_profile;

mod flags;
mod units;

use bench_profile::Profile;

/// One benchmark arm: broker durability tier and optional ordered horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Arm {
    pub tier: bench_spec::Tier,
    /// Ordered-tier horizon in milliseconds (`Some` iff `tier` is ordered).
    pub horizon_ms: Option<u64>,
}

impl Arm {
    /// THE DEFAULT ARM, NAMED: what aterm opens its guarded broker at by
    fn shipped() -> Arm {
        let tier = bench_spec::Tier::default();
        Arm {
            tier,
            horizon_ms: (tier == bench_spec::Tier::Ordered)
                .then_some(bench_spec::ORDERED_HORIZON_MS),
        }
    }

    /// The arm's words for the bench (`durability=… [horizon_ms=…]`).
    fn words(self) -> Vec<String> {
        let mut words = vec![format!("durability={}", self.tier.word())];
        if let Some(ms) = self.horizon_ms {
            words.push(format!("horizon_ms={ms}"));
        }
        words
    }

    /// The arm's flags for the ledger's reproducing line.
    fn flags(self) -> String {
        let mut flags = format!(" --durability {}", self.tier.word());
        if let Some(ms) = self.horizon_ms {
            flags.push_str(&format!(" --horizon-ms {ms}"));
        }
        flags
    }
}

/// Parsed `verify perf fabric` CLI options.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    /// Scenarios to measure (`--scenario`).
    pub scenario: bench_spec::Scenario,
    pub samples: Option<u32>,
    pub burst: Option<u32>,
    /// Ping-pong measured round trips (`--pingpong-rounds`).
    pub pingpong: Option<u32>,
    pub attach: Option<Vec<u32>>,
    pub rounds: Option<u32>,
    /// Arms to measure in order (`--durability` and `--horizon-ms`).
    pub arms: Vec<Arm>,
    /// Number of alternating passes over `arms` (`--passes`).
    pub passes: u32,
    pub json_out: Option<PathBuf>,
    /// Cargo profile to measure (`--profile`).
    pub profile: Profile,
    /// Broker log retention override (`--retention on|off`).
    pub retention: Option<bool>,
    /// Bridge bus mirror mode (`--log auto|off`).
    pub log: Option<bench_spec::Log>,
    /// Retaining broker tick interval override in milliseconds (`--retention-interval-ms`).
    pub retention_interval_ms: Option<u64>,
    /// Idle bystander sessions spawned alongside measured sessions (`--bystanders`).
    pub bystanders: Option<u32>,
}

fn positive(flag: &str, raw: &str) -> Result<u32, String> {
    match raw.parse::<u32>() {
        Ok(v) if v > 0 => Ok(v),
        _ => Err(format!("`{flag}` needs a positive integer, got `{raw}`")),
    }
}

impl Args {
    /// Parse `verify perf fabric` CLI options.
    ///
    /// # Errors
    /// Returns an error string if an option value or combination is rejected by the benchmark grammar.
    pub(crate) fn from_options(options: &BTreeMap<&'static str, String>) -> Result<Args, String> {
        let mut a = Args {
            passes: 1,
            ..Args::default()
        };
        let mut tiers: Vec<bench_spec::Tier> = Vec::new();
        let mut horizons: Vec<u64> = Vec::new();
        for (flag, raw) in options {
            match *flag {
                "--scenario" => {
                    a.scenario = bench_spec::Scenario::parse(raw).map_err(|_| {
                        format!("`--scenario` takes all, messaging, pingpong, attach or handoff, not `{raw}`")
                    })?;
                }
                "--latency-samples" => a.samples = Some(positive(flag, raw)?),
                "--burst" => a.burst = Some(positive(flag, raw)?),
                "--pingpong-rounds" => a.pingpong = Some(positive(flag, raw)?),
                "--attach-rounds" => a.rounds = Some(positive(flag, raw)?),
                "--passes" => a.passes = positive(flag, raw)?,
                "--attach-sessions" => {
                    a.attach = Some(raw.split(',').map(|n| positive(flag, n)).collect::<Result<
                        Vec<u32>,
                        String,
                    >>(
                    )?);
                }
                "--durability" => {
                    tiers = raw
                        .split(',')
                        .map(|word| {
                            bench_spec::Tier::parse(word).map_err(|_| {
                                format!(
                                    "`--durability` takes strict, ordered or relaxed, \
                                     comma-separated, not `{raw}`"
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--horizon-ms" => {
                    horizons = raw
                        .split(',')
                        .map(|ms| {
                            ms.parse::<u64>().map_err(|_| {
                                format!(
                                    "`--horizon-ms` takes milliseconds, comma-separated, not \
                                     `{raw}`"
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--json-out" => a.json_out = Some(PathBuf::from(raw)),
                "--profile" => a.profile = Profile::parse(raw)?,
                "--retention" => {
                    a.retention = Some(match raw.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => {
                            return Err(format!("`--retention` takes on or off, not `{raw}`"));
                        }
                    });
                }
                "--log" => {
                    a.log = Some(
                        bench_spec::Log::parse(raw)
                            .map_err(|_| format!("`--log` takes auto or off, not `{raw}`"))?,
                    );
                }
                "--retention-interval-ms" => {
                    a.retention_interval_ms = Some(u64::from(positive(flag, raw)?));
                }
                "--bystanders" => {
                    a.bystanders = Some(raw.parse::<u32>().map_err(|_| {
                        format!("`{flag}` needs a non-negative integer, got `{raw}`")
                    })?);
                }
                "--no-windows" => {}
                other => {
                    return Err(format!(
                        "internal: `{other}` has no parser in the fabric lane"
                    ));
                }
            }
        }
        if !horizons.is_empty() && !tiers.contains(&bench_spec::Tier::Ordered) {
            return Err(
                "`--horizon-ms` is the ordered arm's: name it in `--durability`".to_string(),
            );
        }
        for tier in tiers {
            match tier {
                bench_spec::Tier::Ordered if !horizons.is_empty() => {
                    a.arms.extend(horizons.iter().map(|&ms| Arm {
                        tier,
                        horizon_ms: Some(ms),
                    }));
                }
                bench_spec::Tier::Ordered => a.arms.push(Arm {
                    tier,
                    horizon_ms: Some(bench_spec::ORDERED_HORIZON_MS),
                }),
                _ => a.arms.push(Arm {
                    tier,
                    horizon_ms: None,
                }),
            }
        }
        for arm in a.plan() {
            bench_spec::parse(&a.bench_words(Path::new("/scratch"), arm))
                .map_err(|why| format!("the fabric bench refuses this run: {why}"))?;
        }
        if a.json_out.is_some() && a.plan().len() > 1 {
            return Err(
                "`--json-out` names one file, and this plan is more than one run: each run's \
                 JSON is its ledger entry's"
                    .to_string(),
            );
        }
        Ok(a)
    }

    /// Build the alternating pass schedule of [`Arm`]s (forward on even passes, reversed on odd passes).
    pub(crate) fn plan(&self) -> Vec<Arm> {
        let arms: Vec<Arm> = if self.arms.is_empty() {
            vec![Arm::shipped()]
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

    /// Format the `FABRIC_BENCH` environment string for `arm` writing to `out`.
    pub(crate) fn bench_words(&self, out: &Path, arm: Arm) -> String {
        let mut words = vec![format!("out={}", out.display())];
        if self.scenario != bench_spec::Scenario::All {
            words.push(format!("scenario={}", self.scenario.word()));
        }
        if let Some(n) = self.samples {
            words.push(format!("samples={n}"));
        }
        if let Some(n) = self.burst {
            words.push(format!("burst={n}"));
        }
        if let Some(n) = self.pingpong {
            words.push(format!("pingpong={n}"));
        }
        if let Some(sizes) = &self.attach {
            let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
            words.push(format!("attach={}", sizes.join(",")));
        }
        if let Some(n) = self.rounds {
            words.push(format!("rounds={n}"));
        }
        if let Some(on) = self.retention {
            words.push(format!("retention={}", if on { "on" } else { "off" }));
        }
        if let Some(log) = self.log {
            words.push(format!("log={}", log.word()));
        }
        if let Some(ms) = self.retention_interval_ms {
            words.push(format!("retention_interval_ms={ms}"));
        }
        if let Some(n) = self.bystanders {
            words.push(format!("bystanders={n}"));
        }
        words.extend(arm.words());
        words.join(" ")
    }

    /// Format the reproducing `verify perf fabric` command line for `arm`.
    pub(crate) fn command_line(&self, arm: Arm) -> String {
        let mut line = String::from("targo --unverified verify perf fabric");
        if self.scenario != bench_spec::Scenario::All {
            line.push_str(&format!(" --scenario {}", self.scenario.word()));
        }
        if self.profile != Profile::Dev {
            line.push_str(&format!(" --profile {}", self.profile.name()));
        }
        if let Some(n) = self.samples {
            line.push_str(&format!(" --latency-samples {n}"));
        }
        if let Some(n) = self.burst {
            line.push_str(&format!(" --burst {n}"));
        }
        if let Some(n) = self.pingpong {
            line.push_str(&format!(" --pingpong-rounds {n}"));
        }
        if let Some(sizes) = &self.attach {
            let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
            line.push_str(&format!(" --attach-sessions {}", sizes.join(",")));
        }
        if let Some(n) = self.rounds {
            line.push_str(&format!(" --attach-rounds {n}"));
        }
        if let Some(on) = self.retention {
            line.push_str(&format!(" --retention {}", if on { "on" } else { "off" }));
        }
        if let Some(log) = self.log {
            line.push_str(&format!(" --log {}", log.word()));
        }
        if let Some(ms) = self.retention_interval_ms {
            line.push_str(&format!(" --retention-interval-ms {ms}"));
        }
        if let Some(n) = self.bystanders {
            line.push_str(&format!(" --bystanders {n}"));
        }
        line.push_str(&arm.flags());
        line
    }

    /// Cargo arguments naming the benchmark target and profile for this run.
    pub(crate) fn build_args(&self) -> Vec<&'static str> {
        vec![
            "--profile",
            self.profile.cargo_profile(),
            "-p",
            "aterm",
            "--bench",
            "fabric_bench",
        ]
    }
}

/// The variable the bench reads its run from.
const BENCH_ENV: &str = "FABRIC_BENCH";

/// The bench's progress lines start with this; the lane echoes them live and
const PROGRESS: &str = "fabric bench:";

/// How many trailing output lines a failure quotes.
const TAIL: usize = 60;

/// Run the `fabric` performance lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    arena::outcome(measure(args))
}

fn measure(args: &Args) -> Result<String, Halt> {
    let ledger_dir = workspace_root().join("docs/measured/fabric");
    if !ledger_dir.join("README.md").is_file() {
        return Err(Halt::skip(
            format!("no fabric ledger at {}", ledger_dir.display()),
            "run the lane in the development checkout, which carries docs/measured/fabric",
        ));
    }
    let targo = Targo::new(crate::perf::driver::cargo_driver()?);
    let profile = args.profile;
    let removed: Vec<String> = std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| flags::is_env_source(key))
        .collect();
    let sources = config_rustflags(&targo)?;
    if let Some(why) = flags::refusal(&sources) {
        return Err(Halt::Fail(format!(
            "the `{}` arm's build is not what the arm says: {why}",
            profile.name()
        )));
    }
    let audit = plan(&targo, args)?;
    if let Some(why) = audit.refusal(profile.every_crate_at()) {
        return Err(Halt::Fail(format!(
            "the `{}` arm's build is not what the arm says: {why}",
            profile.name()
        )));
    }
    let mut build = format!(
        "`{}` — {}. Cargo's plan, read before the run: {}; {}",
        profile.name(),
        profile.describe(),
        audit.summary(),
        flags::describe(&sources)
    );
    if !removed.is_empty() {
        build.push_str(&format!(" (the caller's {} removed)", removed.join(", ")));
    }
    let pins: Vec<String> = targo
        .git_facts
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    build.push_str(&format!(
        "; the build's git facts pinned for every run: {}",
        pins.join(" ")
    ));
    eprintln!("  {build}");
    let load_before = load_average();
    let built = build_bench(&targo, args)?;
    if Profile::of_binary(&built) != Some(profile) {
        return Err(Halt::Fail(format!(
            "cargo built the `{}` arm's aterm at {}, outside the arm's `{}` directory",
            profile.name(),
            built.display(),
            profile.dir()
        )));
    }
    let built_identity = bench_profile::identity(&built)
        .map_err(|why| Halt::Fail(format!("the aterm cargo just built cannot be read: {why}")))?;
    let built = Built {
        path: built,
        identity: built_identity,
        lane_build: build,
    };
    let plan = args.plan();
    let mut written = Vec::with_capacity(plan.len());
    let mut load = format!("{load_before} before the build");
    for (i, arm) in plan.iter().copied().enumerate() {
        if plan.len() > 1 {
            eprintln!(
                "  run {} of {}: {}",
                i + 1,
                plan.len(),
                arm.words().join(" ")
            );
        }
        let entry = run_arm(&targo, args, arm, &built, &ledger_dir, &load)?;
        written.push(entry.display().to_string());
        load = format!("{} before the run", load_average());
    }
    Ok(format!("ledger entries {}", written.join(", ")))
}

/// The binary the lane built for the arm, and what it read about the build.
struct Built {
    path: PathBuf,
    identity: String,
    lane_build: String,
}

/// One bench run of `arm` on the binary `built`, written into the ledger: the
fn run_arm(
    targo: &Targo<'_>,
    args: &Args,
    arm: Arm,
    built: &Built,
    ledger_dir: &Path,
    load_before: &str,
) -> Result<PathBuf, Halt> {
    let profile = args.profile;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-fabric-")
        .tempdir()
        .map_err(|e| format!("could not make a scratch directory: {e}"))?;
    let words = args.bench_words(scratch.path(), arm);
    let mut bench_args = vec!["-q"];
    bench_args.extend(args.build_args());
    eprintln!(
        "  $ {BENCH_ENV}=\"{words}\" {}",
        targo.display("bench", &bench_args)
    );
    let mut child = targo
        .command("bench")
        .args(bench_args)
        .env(BENCH_ENV, &words)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run targo: {e}"))?;
    let stdout = child.stdout.take().map(|out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::BufReader::new(out), &mut text);
            text
        })
    });
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
            if line.starts_with(PROGRESS) {
                eprintln!("  {line}");
            }
            tail.push(line);
            if tail.len() > TAIL {
                tail.remove(0);
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("could not wait for targo: {e}"))?;
    let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
    if !status.success() {
        let out_tail: Vec<&str> = stdout.lines().rev().take(TAIL).collect();
        return Err(Halt::Fail(format!(
            "the fabric bench exited {:?}\n--- stderr (tail) ---\n{}\n--- stdout (tail) ---\n{}",
            status.code(),
            tail.join("\n"),
            out_tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    let read = |name: &str| {
        let path = scratch.path().join(name);
        std::fs::read_to_string(&path)
            .map_err(|e| format!("the bench wrote no {}: {e}", path.display()))
    };
    let tables = read("fabric.md")?;
    let mut report: aterm_json::Value = aterm_json::from_str(&read("fabric.json")?)
        .map_err(|e| format!("the bench's fabric.json does not parse: {e}"))?;
    if let Some(why) = another_build(&report, &built.path, &built.identity) {
        return Err(Halt::Fail(format!(
            "{why}: these numbers are not the `{}` arm's build, and no ledger entry was written",
            profile.name()
        )));
    }
    if report.get("scenario").and_then(aterm_json::Value::as_str) != Some(args.scenario.word()) {
        return Err(Halt::Fail(format!(
            "the fabric bench did not report the requested scenario `{}`; no ledger entry was written",
            args.scenario.word()
        )));
    }
    if let Some(fields) = report.as_object_mut() {
        fields.insert("lane_build".to_string(), built.lane_build.clone().into());
    }
    let json = aterm_json::to_string(&report)
        .map_err(|e| format!("the bench's report does not serialize: {e}"))?;
    let load = format!("{load_before}, {} after the run", load_average());
    eprintln!("{tables}");

    let date = super::trend::utc_date();
    let header = Provenance {
        scenario: args.scenario,
        date: &date,
        machine: &arena::chip(),
        macos: &arena::macos_version(),
        aterm: &arena::git_provenance(&workspace_root()),
        astream: &arena::git_provenance(&workspace_root().join("vendor/astream")),
        spec: report
            .get("spec")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("?"),
        profile: report
            .get("profile")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("?"),
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
    let (md_path, json_path) = ledger_paths(ledger_dir, &date);
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

/// Every targo this lane runs: the run's toolchain ([`CargoDriver`]), in the
struct Targo<'a> {
    driver: &'a CargoDriver,
    build_dir: Option<String>,
    git_facts: Vec<(&'static str, String)>,
}

impl<'a> Targo<'a> {
    fn new(driver: &'a CargoDriver) -> Self {
        Targo {
            driver,
            build_dir: crate::perf::driver::workspace_build_dir().map(str::to_string),
            git_facts: git_facts(
                |key| std::env::var(key).ok(),
                || probe_git_facts(&workspace_root()),
            ),
        }
    }

    /// The arguments before the caller's: the cache `--config`, the lane flag,
    fn lead(&self, verb: &str) -> Vec<String> {
        let mut lead = Vec::new();
        if let Some(value) = &self.build_dir {
            lead.extend(["--config".to_string(), value.clone()]);
        }
        lead.extend(
            CargoDriver::lane_args(verb)
                .iter()
                .map(|s| (*s).to_string()),
        );
        lead.push(verb.to_string());
        lead
    }

    fn command(&self, verb: &str) -> Command {
        let mut cmd = aterm_uds::spawnfd::command(&self.driver.program);
        cmd.args(self.lead(verb)).current_dir(workspace_root());
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(flags::is_env_source) {
                cmd.env_remove(key);
            }
        }
        for (key, value) in &self.git_facts {
            cmd.env(key, value);
        }
        cmd
    }

    /// The command line [`Self::command`] builds, for the `$ …` log line.
    fn display(&self, verb: &str, args: &[&str]) -> String {
        let mut parts = vec![self.driver.program.display().to_string()];
        parts.extend(self.lead(verb));
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }

    /// Run `verb args` to completion; its stdout, or a failure quoting its
    fn output(&self, verb: &str, args: &[&str], what: &str) -> Result<String, Halt> {
        let out = self
            .command(verb)
            .args(args)
            .output()
            .map_err(|e| format!("could not run targo for {what}: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(TAIL).collect();
            return Err(Halt::Fail(format!(
                "`{}` exited {:?}\n{}",
                self.display(verb, args),
                out.status.code(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The three facts `crates/aterm/build.rs` stamps `aterm` with that it would
const GIT_FACTS: [&str; 3] = [
    "ATERM_BUILD_GIT_COMMIT",
    "ATERM_BUILD_GIT_COMMIT_FULL",
    "ATERM_BUILD_DEV_COMMITS",
];

/// THE BUILD'S GIT FACTS, PINNED FOR THE WHOLE PLAN: the caller's when it
fn git_facts(
    caller: impl Fn(&str) -> Option<String>,
    probe: impl FnOnce() -> [String; 3],
) -> Vec<(&'static str, String)> {
    let pinned: Vec<String> = GIT_FACTS
        .iter()
        .filter_map(|key| caller(key).filter(|v| !v.trim().is_empty()))
        .collect();
    let values = match <[String; 3]>::try_from(pinned) {
        Ok(all) => all,
        Err(_) => probe(),
    };
    GIT_FACTS.into_iter().zip(values).collect()
}

/// build.rs's own reading of the three facts in `dir`, probe for probe
fn probe_git_facts(dir: &Path) -> [String; 3] {
    let git = |args: &[&str]| {
        arena::stdout_of(
            aterm_uds::spawnfd::command("git")
                .args(args)
                .current_dir(dir),
        )
        .filter(|s| !s.is_empty())
    };
    let short = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_some();
    let commit = if short != "unknown" && dirty {
        format!("{short}-dirty")
    } else {
        short
    };
    let full = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dev_commits = git(&["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"])
        .and_then(|tag| git(&["rev-list", &format!("{}..HEAD", tag.trim()), "--count"]))
        .unwrap_or_else(|| "0".into());
    [commit, full, dev_commits]
}

/// Cargo's plan for this arm's build of the bench (`--unit-graph`, which
fn plan(targo: &Targo<'_>, args: &Args) -> Result<units::Audit, Halt> {
    let mut plan_args = args.build_args();
    plan_args.extend(["--unit-graph", "-Z", "unstable-options"]);
    let graph = targo.output("bench", &plan_args, "the build plan")?;
    units::Audit::of(&graph)
        .map_err(|why| Halt::Fail(format!("the build plan does not read: {why}")))
}

/// The rustflags cargo's config files hold (`targo config get` of the `build`
fn config_rustflags(targo: &Targo<'_>) -> Result<Vec<flags::Source>, Halt> {
    let table = |name: &str| -> Result<Option<String>, Halt> {
        let args = ["-Z", "unstable-options", "get", "--format", "json", name];
        let out = targo
            .command("config")
            .args(args)
            .output()
            .map_err(|e| format!("could not run targo config get: {e}"))?;
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
        } else if err.contains("is not set") {
            Ok(None)
        } else {
            Err(Halt::Fail(format!(
                "`{}` exited {:?}\n{err}",
                targo.display("config", &args),
                out.status.code()
            )))
        }
    };
    flags::config_sources(table("build")?.as_deref(), table("target")?.as_deref())
        .map_err(|why| Halt::Fail(format!("cargo's config does not read: {why}")))
}

/// Build the arm's bench and the `aterm` it launches, and return where cargo
fn build_bench(targo: &Targo<'_>, args: &Args) -> Result<PathBuf, Halt> {
    let mut build_args = vec!["-q"];
    build_args.extend(args.build_args());
    build_args.extend(["--no-run", "--message-format", "json-render-diagnostics"]);
    eprintln!("  $ {}", targo.display("bench", &build_args));
    let messages = targo.output("bench", &build_args, "the bench's build")?;
    built_binary(&messages, "aterm").map_err(|why| Halt::Fail(format!("the bench's build: {why}")))
}

/// The executable cargo reports for the binary target `name`, out of a
fn built_binary(messages: &str, name: &str) -> Result<PathBuf, String> {
    for line in messages.lines() {
        let Ok(message) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let target = message.get("target");
        let named = target
            .and_then(|t| t.get("name"))
            .and_then(aterm_json::Value::as_str)
            == Some(name);
        let bin = target
            .and_then(|t| t.get("kind"))
            .and_then(aterm_json::Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")));
        if named && bin {
            return message
                .get("executable")
                .and_then(aterm_json::Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| format!("cargo built the `{name}` binary and named no executable"));
        }
    }
    Err(format!("cargo reported no `{name}` binary"))
}

/// Why the bench's report is not of the binary the lane built — another path,
fn another_build(report: &aterm_json::Value, built: &Path, identity: &str) -> Option<String> {
    let ran = report
        .get("aterm_binary")
        .and_then(aterm_json::Value::as_str)
        .unwrap_or("(none reported)");
    if Path::new(ran) != built {
        return Some(format!(
            "the bench launched {ran}, not the {} cargo just built",
            built.display()
        ));
    }
    let seen = report.get("aterm_binary_identity");
    for moment in ["start", "end"] {
        let at = seen
            .and_then(|s| s.get(moment))
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("(none reported)");
        if at != identity {
            return Some(format!(
                "{ran} at the bench's {moment} was {at}, not the build's {identity}: another build \
                 wrote that path"
            ));
        }
    }
    None
}

/// `<dir>/<date>-fabric.md` and its `.json`, or `-2`, `-3`… when a run that
fn ledger_paths(dir: &Path, date: &str) -> (PathBuf, PathBuf) {
    let mut n = 1;
    loop {
        let stem = if n == 1 {
            format!("{date}-fabric")
        } else {
            format!("{date}-fabric-{n}")
        };
        let md = dir.join(format!("{stem}.md"));
        let json = dir.join(format!("{stem}.json"));
        if !md.exists() && !json.exists() {
            return (md, json);
        }
        n += 1;
    }
}

/// `sysctl -n vm.loadavg`'s three averages (`1.0 2.0 3.0`), or `unavailable`.
fn load_average() -> String {
    arena::stdout_of(aterm_uds::spawnfd::command("sysctl").args(["-n", "vm.loadavg"]))
        .map(|s| {
            s.trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unavailable".to_string())
}

/// The cores a load average is read against: logical CPUs and, where the OS
fn cpus() -> String {
    let logical = std::thread::available_parallelism()
        .map_or_else(|_| "?".to_string(), |n| n.get().to_string());
    match arena::stdout_of(
        aterm_uds::spawnfd::command("sysctl").args(["-n", "hw.perflevel0.physicalcpu"]),
    )
    .filter(|s| !s.is_empty())
    {
        Some(performance) => format!("{logical} logical, {performance} performance cores"),
        None => format!("{logical} logical"),
    }
}

/// What a ledger entry records about the run beside its numbers.
struct Provenance<'a> {
    scenario: bench_spec::Scenario,
    date: &'a str,
    machine: &'a str,
    macos: &'a str,
    aterm: &'a str,
    astream: &'a str,
    spec: &'a str,
    profile: &'a str,
    elapsed_s: f64,
    command: &'a str,
    build: &'a str,
    thermal: &'a str,
    cpus: &'a str,
    load: &'a str,
}

impl Provenance<'_> {
    /// The entry's head. Pure (unit-tested).
    fn render(&self) -> String {
        let scenarios = match self.scenario {
            bench_spec::Scenario::All => "post -> deliver, burst, ping-pong, attach",
            selected => selected.word(),
        };
        format!(
            "<!-- SPDX-License-Identifier: Apache-2.0 -->\n\
             <!-- Copyright 2026 Andrew Yates -->\n\
             \n\
             # Fabric: {scenarios} — {date}\n\
             \n\
             Protocol: `docs/measured/fabric/README.md`. MEASURED, not judged. Append-only: a \
             superseding run gets a new file.\n\
             \n\
             - **Machine:** {machine}\n\
             - **macOS:** {macos}\n\
             - **aterm:** {aterm} ({profile})\n\
             - **astream:** vendor/astream at {astream}\n\
             - **Run:** `{command}` ({spec}), {elapsed:.1} s\n\
             - **Build:** {build}\n\
             - **Thermal:** {thermal}\n\
             - **CPUs:** {cpus}\n\
             - **Load average (1/5/15 min):** {load}\n",
            date = self.date,
            machine = self.machine,
            macos = self.macos,
            aterm = self.aterm,
            profile = self.profile,
            astream = self.astream,
            command = self.command,
            spec = self.spec,
            elapsed = self.elapsed_s,
            build = self.build,
            thermal = self.thermal,
            cpus = self.cpus,
            load = self.load,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_become_the_bench_words_and_bad_values_are_refused() {
        let mut o = BTreeMap::new();
        o.insert("--latency-samples", "50".to_string());
        o.insert("--attach-sessions", "1,4,16".to_string());
        o.insert("--no-windows", String::new());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.scenario, bench_spec::Scenario::All);
        assert_eq!(a.samples, Some(50));
        assert_eq!(a.attach, Some(vec![1, 4, 16]));
        let shipped = Arm {
            tier: bench_spec::Tier::Ordered,
            horizon_ms: Some(bench_spec::ORDERED_HORIZON_MS),
        };
        assert_eq!(
            a.plan(),
            [shipped],
            "aterm's default for its guarded broker"
        );
        assert_eq!(
            a.bench_words(Path::new("/s"), shipped),
            "out=/s samples=50 attach=1,4,16 durability=ordered horizon_ms=100"
        );
        assert_eq!(
            a.command_line(shipped),
            "targo --unverified verify perf fabric --latency-samples 50 --attach-sessions 1,4,16 \
             --durability ordered --horizon-ms 100"
        );
        o.insert("--durability", "relaxed".to_string());
        let a = Args::from_options(&o).expect("parses");
        let relaxed = Arm {
            tier: bench_spec::Tier::Relaxed,
            horizon_ms: None,
        };
        assert_eq!(a.plan(), [relaxed]);
        assert_eq!(
            a.bench_words(Path::new("/s"), relaxed),
            "out=/s samples=50 attach=1,4,16 durability=relaxed"
        );
        assert!(
            a.command_line(relaxed).ends_with(" --durability relaxed"),
            "{}",
            a.command_line(relaxed)
        );
        o.insert("--pingpong-rounds", "40".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.pingpong, Some(40));
        assert_eq!(
            a.bench_words(Path::new("/s"), relaxed),
            "out=/s samples=50 pingpong=40 attach=1,4,16 durability=relaxed"
        );
        assert!(
            a.command_line(relaxed).contains(" --pingpong-rounds 40 "),
            "{}",
            a.command_line(relaxed)
        );
        assert_eq!(
            Args::from_options(&BTreeMap::new())
                .expect("parses")
                .bench_words(Path::new("/s"), shipped),
            "out=/s durability=ordered horizon_ms=100"
        );
        for word in ["all", "messaging", "pingpong", "attach", "handoff"] {
            let mut selected = BTreeMap::new();
            selected.insert("--scenario", word.to_string());
            let a = Args::from_options(&selected).expect(word);
            let spec =
                bench_spec::parse(&a.bench_words(Path::new("/s"), shipped)).expect("bench words");
            assert_eq!(spec.scenario.word(), word);
            if word == "all" {
                assert_eq!(
                    a.bench_words(Path::new("/s"), shipped),
                    "out=/s durability=ordered horizon_ms=100"
                );
                assert!(!a.command_line(shipped).contains("--scenario"));
            } else {
                assert!(
                    a.command_line(shipped)
                        .contains(&format!(" --scenario {word} "))
                );
            }
        }
        for (flag, bad) in [
            ("--burst", "0"),
            ("--burst", "x"),
            ("--burst", "6000"),
            ("--pingpong-rounds", "0"),
            ("--pingpong-rounds", "many"),
            ("--attach-sessions", "8,1"),
            ("--attach-sessions", "1,,2"),
            ("--attach-sessions", "1,200"),
            ("--attach-rounds", "-1"),
            ("--durability", "replicated"),
            ("--durability", "Relaxed"),
            ("--durability", "strict,,relaxed"),
            ("--horizon-ms", "100"),
            ("--passes", "0"),
            ("--scenario", ""),
            ("--scenario", "Pingpong"),
            ("--scenario", "pingpong,attach"),
            ("--scenario", "unknown"),
        ] {
            let mut o = BTreeMap::new();
            o.insert(flag, bad.to_string());
            assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
        }
        let mut o = BTreeMap::new();
        o.insert("--burst", bench_spec::BURST_MAX.to_string());
        o.insert("--attach-sessions", format!("1,{}", bench_spec::ATTACH_MAX));
        assert!(Args::from_options(&o).is_ok());
        let mut o = BTreeMap::new();
        o.insert("--warmup", "3".to_string());
        assert!(Args::from_options(&o).is_err());
    }

    /// WHAT SHIPS, NAMED APART: `--retention`, `--log` and
    #[test]
    fn retention_the_mirror_and_the_tick_reach_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        let mut o = BTreeMap::new();
        o.insert("--retention", "on".to_string());
        o.insert("--log", "off".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(
            (a.retention, a.log),
            (Some(true), Some(bench_spec::Log::Off))
        );
        assert_eq!(
            a.bench_words(Path::new("/s"), shipped),
            "out=/s retention=on log=off durability=ordered horizon_ms=100"
        );
        assert_eq!(
            a.command_line(shipped),
            "targo --unverified verify perf fabric --retention on --log off --durability ordered \
             --horizon-ms 100"
        );
        let mut o = BTreeMap::new();
        o.insert("--retention-interval-ms", "1000".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(
            a.bench_words(Path::new("/s"), shipped),
            "out=/s retention_interval_ms=1000 durability=ordered horizon_ms=100"
        );
        assert!(
            a.command_line(shipped)
                .contains(" --retention-interval-ms 1000 "),
            "{}",
            a.command_line(shipped)
        );
        for pairs in [
            &[("--retention", "yes")][..],
            &[("--log", "turns")],
            &[("--log", "")],
            &[("--retention-interval-ms", "0")],
            &[("--retention", "off"), ("--retention-interval-ms", "1000")],
        ] {
            let o: BTreeMap<&'static str, String> =
                pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect();
            assert!(Args::from_options(&o).is_err(), "{pairs:?}");
        }
    }

    /// `--bystanders N` (B10) reaches the bench's words and the entry's
    #[test]
    fn the_bystanders_reach_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        for n in ["0", "64"] {
            let mut o = BTreeMap::new();
            o.insert("--bystanders", n.to_string());
            let a = Args::from_options(&o).expect("parses");
            assert_eq!(
                a.bench_words(Path::new("/s"), shipped),
                format!("out=/s bystanders={n} durability=ordered horizon_ms=100")
            );
            assert!(
                a.command_line(shipped)
                    .contains(&format!(" --bystanders {n} ")),
                "{}",
                a.command_line(shipped)
            );
        }
        let a = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(
            !a.bench_words(Path::new("/s"), shipped)
                .contains("bystanders")
        );
        for bad in ["-1", "many", "999"] {
            let mut o = BTreeMap::new();
            o.insert("--bystanders", bad.to_string());
            assert!(Args::from_options(&o).is_err(), "{bad}");
        }
    }

    /// THE SAME-DAY ARMS IN ONE INVOCATION: `--durability` names the tiers in
    #[test]
    fn the_arms_run_in_order_and_alternate_by_pass() {
        use bench_spec::Tier;
        let arm = |tier, horizon_ms| Arm { tier, horizon_ms };
        let mut o = BTreeMap::new();
        o.insert("--durability", "strict,ordered,relaxed".to_string());
        o.insert("--horizon-ms", "1000,100".to_string());
        let a = Args::from_options(&o).expect("parses");
        let (s, o1, o2, r) = (
            arm(Tier::Strict, None),
            arm(Tier::Ordered, Some(1000)),
            arm(Tier::Ordered, Some(100)),
            arm(Tier::Relaxed, None),
        );
        assert_eq!(a.plan(), [s, o1, o2, r]);
        assert_eq!(
            a.bench_words(Path::new("/s"), o2),
            "out=/s durability=ordered horizon_ms=100"
        );
        assert_eq!(
            a.command_line(o2),
            "targo --unverified verify perf fabric --durability ordered --horizon-ms 100"
        );
        o.insert("--passes", "2".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.plan(), [s, o1, o2, r, r, o2, o1, s]);
        let mut bare = BTreeMap::new();
        bare.insert("--durability", "ordered".to_string());
        let a = Args::from_options(&bare).expect("parses");
        assert_eq!(
            a.plan(),
            [arm(Tier::Ordered, Some(bench_spec::ORDERED_HORIZON_MS))]
        );
        bare.insert("--json-out", "/tmp/f.json".to_string());
        assert!(Args::from_options(&bare).is_ok(), "one run, one file");
        let mut shipped = BTreeMap::new();
        shipped.insert("--json-out", "/tmp/f.json".to_string());
        assert!(
            Args::from_options(&shipped).is_ok(),
            "no tier named: one run"
        );
        o.insert("--json-out", "/tmp/f.json".to_string());
        assert!(Args::from_options(&o).is_err(), "eight runs, one file");
    }

    /// THE BUILD'S GIT FACTS ARE ONE READING, all three pinned on every
    #[test]
    fn the_build_git_facts_are_the_callers_three_or_one_probe() {
        let probe = || {
            [
                "0123456789ab-dirty".to_string(),
                "0123456789abcdef".to_string(),
                "7".to_string(),
            ]
        };
        let pinned = git_facts(
            |key| Some(format!("pinned {key}")),
            || panic!("probed beside the caller's three pins"),
        );
        assert_eq!(
            pinned.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            GIT_FACTS
        );
        assert!(
            pinned
                .iter()
                .all(|(key, value)| *value == format!("pinned {key}")),
            "{pinned:?}"
        );
        let partials: [fn(&str) -> Option<String>; 3] = [
            |key: &str| (key != "ATERM_BUILD_DEV_COMMITS").then(|| "x".to_string()),
            |key: &str| {
                Some(if key == "ATERM_BUILD_GIT_COMMIT" {
                    " ".to_string()
                } else {
                    "x".to_string()
                })
            },
            |_: &str| None,
        ];
        for partial in partials {
            let facts = git_facts(partial, probe);
            assert_eq!(
                facts
                    .into_iter()
                    .map(|(_, value)| value)
                    .collect::<Vec<_>>(),
                probe(),
                "short of three pins, all three are probed"
            );
        }
    }

    #[test]
    fn the_profile_option_picks_the_arm_its_build_and_its_command_line() {
        let dev = Args::from_options(&BTreeMap::new()).expect("parses");
        assert_eq!(dev.profile, Profile::Dev);
        assert_eq!(
            dev.build_args(),
            [
                "--profile",
                "test",
                "-p",
                "aterm",
                "--bench",
                "fabric_bench"
            ]
        );
        assert_eq!(
            dev.command_line(Arm::shipped()),
            "targo --unverified verify perf fabric --durability ordered --horizon-ms 100"
        );
        let mut o = BTreeMap::new();
        o.insert("--profile", "dev".to_string());
        assert_eq!(Args::from_options(&o), Ok(dev));

        let mut o = BTreeMap::new();
        o.insert("--profile", "opt".to_string());
        o.insert("--burst", "150".to_string());
        let opt = Args::from_options(&o).expect("parses");
        assert_eq!(opt.profile, Profile::Opt);
        assert_eq!(
            opt.build_args(),
            [
                "--profile",
                "opt-test",
                "-p",
                "aterm",
                "--bench",
                "fabric_bench"
            ]
        );
        assert_eq!(
            opt.command_line(Arm::shipped()),
            "targo --unverified verify perf fabric --profile opt --burst 150 --durability ordered \
             --horizon-ms 100"
        );
        assert_eq!(
            opt.bench_words(Path::new("/s"), Arm::shipped()),
            "out=/s burst=150 durability=ordered horizon_ms=100"
        );

        for bad in ["", "release", "opt-test", "OPT"] {
            let mut o = BTreeMap::new();
            o.insert("--profile", bad.to_string());
            let why = Args::from_options(&o).expect_err(bad);
            assert!(why.contains("`dev` or `opt`"), "{bad}: {why}");
        }
    }

    #[test]
    fn the_built_aterm_is_the_binary_cargo_reports() {
        let messages = [
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm"},"executable":null,"fresh":true}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"filenames":["/w/target/opt-test/aterm"],"executable":"/w/target/opt-test/aterm","fresh":false}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bench"],"name":"fabric_bench"},"executable":"/c/.cargo-build/opt-test/deps/fabric_bench-7b2c","fresh":false}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not json: a stray line",
        ]
        .join("\n");
        assert_eq!(
            built_binary(&messages, "aterm"),
            Ok(PathBuf::from("/w/target/opt-test/aterm"))
        );
        assert!(built_binary(&messages, "aterm-ctl").is_err());
        let nameless = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"executable":null}"#;
        assert!(built_binary(nameless, "aterm").is_err());
    }

    #[test]
    fn a_report_of_another_path_or_another_file_there_is_another_build() {
        let built = Path::new("/w/target/opt-test/aterm");
        let id = "87127144 bytes, modified 1792221563.000000001";
        let report = |path: &str, start: &str, end: &str| {
            aterm_json::json!({
                "aterm_binary": path,
                "aterm_binary_identity": { "start": start, "end": end },
            })
        };
        let path = built.to_str().expect("utf-8");
        assert_eq!(another_build(&report(path, id, id), built, id), None);
        let why =
            another_build(&report("/w/target/debug/aterm", id, id), built, id).expect("refused");
        assert!(why.contains("/w/target/debug/aterm"), "{why}");
        let other = "84000000 bytes, modified 1792221999.000000001";
        for (start, end, moment) in [(other, id, "start"), (id, other, "end")] {
            let why = another_build(&report(path, start, end), built, id).expect("refused");
            assert!(why.contains(moment) && why.contains(other), "{why}");
        }
        let bare = aterm_json::json!({ "aterm_binary": path });
        assert!(another_build(&bare, built, id).is_some());
    }

    #[test]
    fn a_second_run_on_one_day_gets_its_own_entry() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let (md, json) = ledger_paths(dir.path(), "2026-10-16");
        assert!(md.ends_with("2026-10-16-fabric.md"), "{}", md.display());
        assert!(json.ends_with("2026-10-16-fabric.json"));
        std::fs::write(&md, "x").expect("write");
        let (md2, json2) = ledger_paths(dir.path(), "2026-10-16");
        assert!(md2.ends_with("2026-10-16-fabric-2.md"), "{}", md2.display());
        assert!(json2.ends_with("2026-10-16-fabric-2.json"));
    }

    #[test]
    fn the_entry_head_carries_every_provenance_field() {
        let p = Provenance {
            scenario: bench_spec::Scenario::All,
            date: "2026-10-16",
            machine: "Apple M5 Max",
            macos: "27.0",
            aterm: "abc123",
            astream: "6aab5d26a405",
            spec: "samples=200",
            profile: "dev",
            elapsed_s: 81.0,
            command: "targo --unverified verify perf fabric",
            build: "`dev` — 96 units on the measured path: opt-level 0 ×87",
            thermal: "No thermal warning level has been recorded",
            cpus: "18 logical, 6 performance cores",
            load: "5.62 4.40 4.08 before the build, 6.00 5.00 4.00 after the run",
        };
        let head = p.render();
        for (field, value) in [
            ("date", p.date),
            ("machine", p.machine),
            ("macos", p.macos),
            ("aterm", p.aterm),
            ("astream", p.astream),
            ("spec", p.spec),
            ("profile", p.profile),
            ("elapsed", "81.0"),
            ("command", p.command),
            ("build", p.build),
            ("thermal", p.thermal),
            ("cpus", p.cpus),
            ("load", p.load),
        ] {
            assert!(head.contains(value), "the head lost the {field}:\n{head}");
        }
    }
}
