// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf fabric` benchmark lane runner and provenance ledger writer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::arena::{self, Halt};
use super::bench_build::{
    self, Built, Provenance, Targo, another_build, cpus, ledger_paths, load_average,
};
use super::{LaneRun, workspace_root};

/// THE BENCH'S OWN GRAMMAR for a run, mounted from the bench (std-only) the way
/// `crate::toolchain` is shared with atpkg: the lane refuses exactly what the
/// bench would — before the build, as a usage error, never as a red lane after
/// one — and no bound (`BURST_MAX`, `ATTACH_MAX`, the rising list) is kept in
/// two places to drift. The `fabric-counts` lane composes its worlds in it too.
#[path = "../../../aterm/tests/fabric_bench/spec.rs"]
#[expect(
    dead_code,
    reason = "the lanes validate with `parse`; the knobs a run reads back and `from_env` are the \
              bench's"
)]
pub(super) mod bench_spec;

use bench_build::bench_profile::Profile;

/// One benchmark arm: broker durability tier and optional ordered horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Arm {
    pub tier: bench_spec::Tier,
    /// Ordered-tier horizon in milliseconds (`Some` iff `tier` is ordered).
    pub horizon_ms: Option<u64>,
    /// Whether the arm's logs are sealed at rest (`--at-rest`), as a fabric root's are: on unless
    /// a run names it off.
    pub at_rest: bool,
    /// Where the arm's brokers run (`--broker-process`): in the bench's process unless a run
    /// names the shipped job's band (`background`) or an `Interactive` job's (`interactive`).
    pub broker_process: bench_spec::BrokerProcess,
}

impl Arm {
    fn shipped() -> Arm {
        let tier = bench_spec::Tier::default();
        Arm {
            tier,
            horizon_ms: (tier == bench_spec::Tier::Ordered)
                .then_some(bench_spec::ORDERED_HORIZON_MS),
            at_rest: true,
            broker_process: bench_spec::BrokerProcess::Inproc,
        }
    }

    fn words(self) -> Vec<String> {
        let mut words = vec![format!("durability={}", self.tier.word())];
        if let Some(ms) = self.horizon_ms {
            words.push(format!("horizon_ms={ms}"));
        }
        if !self.at_rest {
            words.push("at_rest=off".to_string());
        }
        if self.broker_process.outside() {
            words.push(format!("broker_process={}", self.broker_process.word()));
        }
        words
    }

    fn flags(self) -> String {
        let mut flags = format!(" --durability {}", self.tier.word());
        if let Some(ms) = self.horizon_ms {
            flags.push_str(&format!(" --horizon-ms {ms}"));
        }
        if !self.at_rest {
            flags.push_str(" --at-rest off");
        }
        if self.broker_process.outside() {
            flags.push_str(&format!(" --broker-process {}", self.broker_process.word()));
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
    /// Broker rate limit override (`--rate-limit on|off`).
    pub rate_limit: Option<bool>,
    /// Bridge bus mirror mode (`--log auto|off`).
    pub log: Option<bench_spec::Log>,
    /// Retaining broker tick interval override in milliseconds (`--retention-interval-ms`).
    pub retention_interval_ms: Option<u64>,
    /// Idle bystander sessions spawned alongside measured sessions (`--bystanders`).
    pub bystanders: Option<u32>,
    /// Posts per leg of the ask scenario (`--asks`).
    pub asks: Option<u32>,
    /// One person-lane post every this many messages in the ask scenario's mix (`--mix-every`).
    pub mix_every: Option<u32>,
    /// The attach world's backlog (`--attach-backlog`): in the bench's process, records behind
    /// every session's refill watermark; with an outside broker, records for each population
    /// seeded into its log before it starts (`bench_spec::Spec::backlog`).
    pub backlog: Option<u32>,
    /// Rows the ask world's asked table is pre-filled to (`--asked-prefill`).
    pub asked_prefill: Option<u32>,
    /// One-way notes the messaging world posts paced after its burst (`--paced-notes`).
    pub paced_notes: Option<u32>,
    /// Both tallies in the messaging, attach, handoff and ask worlds (`--tally`).
    pub tally: bool,
}

fn positive(flag: &str, raw: &str) -> Result<u32, String> {
    match raw
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| raw.parse::<u32>().ok())
        .flatten()
    {
        Some(v) if v > 0 => Ok(v),
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
        let mut at_rest: Vec<bool> = Vec::new();
        let mut processes: Vec<bench_spec::BrokerProcess> = Vec::new();
        for (flag, raw) in options {
            match *flag {
                "--at-rest" => {
                    at_rest = raw
                        .split(',')
                        .map(|word| match word {
                            "on" => Ok(true),
                            "off" => Ok(false),
                            _ => Err(format!(
                                "`--at-rest` takes on or off, comma-separated, not `{raw}`"
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--broker-process" => {
                    processes = raw
                        .split(',')
                        .map(|word| {
                            bench_spec::BrokerProcess::parse(word).map_err(|_| {
                                format!(
                                    "`--broker-process` takes inproc, background or \
                                     interactive, comma-separated, not `{raw}`"
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                "--scenario" => {
                    a.scenario = bench_spec::Scenario::parse(raw).map_err(|_| {
                        format!(
                            "`--scenario` takes all, messaging, pingpong, attach, handoff or ask, \
                             not `{raw}`"
                        )
                    })?;
                }
                "--asks" => a.asks = Some(positive(flag, raw)?),
                "--mix-every" => a.mix_every = Some(positive(flag, raw)?),
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
                            ms.bytes()
                                .all(|b| b.is_ascii_digit())
                                .then(|| ms.parse::<u64>().ok())
                                .flatten()
                                .ok_or_else(|| {
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
                "--rate-limit" => {
                    a.rate_limit = Some(match raw.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => {
                            return Err(format!("`--rate-limit` takes on or off, not `{raw}`"));
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
                    a.bystanders = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--attach-backlog" => {
                    a.backlog = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--asked-prefill" => {
                    a.asked_prefill = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--paced-notes" => {
                    a.paced_notes = Some(
                        raw.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| raw.parse::<u32>().ok())
                            .flatten()
                            .ok_or_else(|| {
                                format!("`{flag}` needs a non-negative integer, got `{raw}`")
                            })?,
                    );
                }
                "--tally" => a.tally = true,
                "--no-windows" | "--admit" => {}
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
                        at_rest: true,
                        broker_process: bench_spec::BrokerProcess::Inproc,
                    }));
                }
                bench_spec::Tier::Ordered => a.arms.push(Arm {
                    tier,
                    horizon_ms: Some(bench_spec::ORDERED_HORIZON_MS),
                    at_rest: true,
                    broker_process: bench_spec::BrokerProcess::Inproc,
                }),
                _ => a.arms.push(Arm {
                    tier,
                    horizon_ms: None,
                    at_rest: true,
                    broker_process: bench_spec::BrokerProcess::Inproc,
                }),
            }
        }
        if !at_rest.is_empty() {
            let arms = if a.arms.is_empty() {
                vec![Arm::shipped()]
            } else {
                std::mem::take(&mut a.arms)
            };
            a.arms = arms
                .into_iter()
                .flat_map(|arm| at_rest.iter().map(move |&at_rest| Arm { at_rest, ..arm }))
                .collect();
        }
        if !processes.is_empty() {
            let arms = if a.arms.is_empty() {
                vec![Arm::shipped()]
            } else {
                std::mem::take(&mut a.arms)
            };
            a.arms = arms
                .into_iter()
                .flat_map(|arm| {
                    processes.iter().map(move |&broker_process| Arm {
                        broker_process,
                        ..arm
                    })
                })
                .collect();
        }
        for arm in a.plan() {
            bench_spec::parse(&a.bench_words(arm), Path::new("/scratch"))
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

    /// Format the `FABRIC_BENCH` measurement knobs for `arm`.
    pub(crate) fn bench_words(&self, arm: Arm) -> String {
        let mut words = Vec::new();
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
        if let Some(on) = self.rate_limit {
            words.push(format!("rate_limit={}", if on { "on" } else { "off" }));
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
        if let Some(n) = self.asks {
            words.push(format!("asks={n}"));
        }
        if let Some(n) = self.mix_every {
            words.push(format!("mix_every={n}"));
        }
        if let Some(n) = self.backlog {
            words.push(format!("backlog={n}"));
        }
        if let Some(n) = self.asked_prefill {
            words.push(format!("asked_prefill={n}"));
        }
        if let Some(n) = self.paced_notes {
            words.push(format!("paced_notes={n}"));
        }
        if self.tally {
            words.push("tally=on".to_string());
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
        if let Some(on) = self.rate_limit {
            line.push_str(&format!(" --rate-limit {}", if on { "on" } else { "off" }));
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
        if let Some(n) = self.asks {
            line.push_str(&format!(" --asks {n}"));
        }
        if let Some(n) = self.mix_every {
            line.push_str(&format!(" --mix-every {n}"));
        }
        if let Some(n) = self.backlog {
            line.push_str(&format!(" --attach-backlog {n}"));
        }
        if let Some(n) = self.asked_prefill {
            line.push_str(&format!(" --asked-prefill {n}"));
        }
        if let Some(n) = self.paced_notes {
            line.push_str(&format!(" --paced-notes {n}"));
        }
        if self.tally {
            line.push_str(" --tally");
        }
        line.push_str(&arm.flags());
        line
    }

    /// Cargo arguments naming the benchmark target and profile for this run.
    pub(crate) fn build_args(&self) -> Vec<&'static str> {
        let mut args = vec![
            "--profile",
            self.profile.cargo_profile(),
            "-p",
            "aterm",
            "--bench",
            "fabric_bench",
        ];
        args.extend_from_slice(self.profile.cargo_features());
        args
    }
}

const PROGRESS: &str = "fabric bench:";

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
    let driver = crate::perf::driver::cargo_driver()?;
    let (targo, built, load_before) =
        bench_build::prepare(driver, args.profile, &args.build_args())?;
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
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| format!("could not make a scratch directory: {e}"))?;
    let words = args.bench_words(arm);
    bench_build::run_bench(
        targo,
        &args.build_args(),
        (bench_spec::ENV, &words),
        (bench_spec::OUT_ENV, scratch.path()),
        PROGRESS,
        "the fabric bench",
        bench_build::Entry::Timed,
    )?;
    let read = |name: &str| {
        let path = scratch.path().join(name);
        crate::read_regular_text(&path)
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
        heading: &heading(args.scenario, arm.broker_process),
        protocol: "docs/measured/fabric/README.md",
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
    let (md_path, json_path) = ledger_paths(ledger_dir, &date, "fabric");
    crate::write_fresh(&md_path, format!("{header}\n{tables}\n"))
        .map_err(|e| format!("could not write {}: {e}", md_path.display()))?;
    crate::write_fresh(&json_path, &json)
        .map_err(|e| format!("could not write {}: {e}", json_path.display()))?;
    eprintln!(
        "ledger entry written: {} (+ {}) — commit both to record the run",
        md_path.display(),
        json_path.display()
    );
    if let Some(out) = &args.json_out {
        super::publication::publish_named(out, &json)
            .map_err(|e| format!("could not write {}: {e}", out.display()))?;
        eprintln!("json written: {}", out.display());
    }
    Ok(md_path)
}

/// An entry's title: the scenarios it measured, and the broker's process when it ran in one of
/// its own, whose `all` is the messaging world without its burst, the ping-pong and the attach
/// (`run/outside.rs`).
fn heading(scenario: bench_spec::Scenario, process: bench_spec::BrokerProcess) -> String {
    let scenarios = match (scenario, process.outside()) {
        (bench_spec::Scenario::All, false) => {
            "post -> deliver, burst, ping-pong, attach, handoff, ask, cross-node"
        }
        (bench_spec::Scenario::All, true) => "post -> deliver, ping-pong, attach",
        (selected, _) => selected.word(),
    };
    if process.outside() {
        format!(
            "Fabric: {scenarios}, on `aterm link broker` as {}",
            process.runs_as()
        )
    } else {
        format!("Fabric: {scenarios}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_output_path_round_trip(out: PathBuf) {
        let args = Args::default();
        let plain = Path::new("/scratch/plain");
        let control = bench_spec::parse(&args.bench_words(Arm::shipped()), plain).unwrap();
        assert_eq!(control.out, plain);
        let decoded = bench_spec::parse(&args.bench_words(Arm::shipped()), &out).unwrap();
        assert_eq!(decoded.out, out);
    }

    #[test]
    fn output_path_with_spaces_round_trips() {
        assert_output_path_round_trip(PathBuf::from("/scratch/with spaces"));
    }

    #[cfg(unix)]
    #[test]
    fn output_path_with_non_utf8_bytes_round_trips() {
        use std::os::unix::ffi::OsStringExt as _;
        assert_output_path_round_trip(
            std::ffi::OsString::from_vec(b"/scratch/non-utf8-\xff".to_vec()).into(),
        );
    }

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
            at_rest: true,
            broker_process: bench_spec::BrokerProcess::Inproc,
        };
        assert_eq!(
            a.plan(),
            [shipped],
            "aterm's default for its guarded broker"
        );
        assert_eq!(
            a.bench_words(shipped),
            "samples=50 attach=1,4,16 durability=ordered horizon_ms=100"
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
            at_rest: true,
            broker_process: bench_spec::BrokerProcess::Inproc,
        };
        assert_eq!(a.plan(), [relaxed]);
        assert_eq!(
            a.bench_words(relaxed),
            "samples=50 attach=1,4,16 durability=relaxed"
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
            a.bench_words(relaxed),
            "samples=50 pingpong=40 attach=1,4,16 durability=relaxed"
        );
        assert!(
            a.command_line(relaxed).contains(" --pingpong-rounds 40 "),
            "{}",
            a.command_line(relaxed)
        );
        assert_eq!(
            Args::from_options(&BTreeMap::new())
                .expect("parses")
                .bench_words(shipped),
            "durability=ordered horizon_ms=100"
        );
        for word in ["all", "messaging", "pingpong", "attach", "handoff", "ask"] {
            let mut selected = BTreeMap::new();
            selected.insert("--scenario", word.to_string());
            let a = Args::from_options(&selected).expect(word);
            let spec =
                bench_spec::parse(&a.bench_words(shipped), Path::new("/s")).expect("bench words");
            assert_eq!(spec.scenario.word(), word);
            if word == "all" {
                assert_eq!(a.bench_words(shipped), "durability=ordered horizon_ms=100");
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
            a.bench_words(shipped),
            "retention=on log=off durability=ordered horizon_ms=100"
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
            a.bench_words(shipped),
            "retention_interval_ms=1000 durability=ordered horizon_ms=100"
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

    /// `--rate-limit off` reaches the bench as `rate_limit=off` and the ledger as the flag that
    /// reproduces it (critic item E06); a word other than on or off is refused.
    #[test]
    fn the_rate_limit_reaches_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        let mut o = BTreeMap::new();
        o.insert("--rate-limit", "off".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.rate_limit, Some(false));
        assert_eq!(
            a.bench_words(shipped),
            "rate_limit=off durability=ordered horizon_ms=100"
        );
        assert_eq!(
            a.command_line(shipped),
            "targo --unverified verify perf fabric --rate-limit off --durability ordered \
             --horizon-ms 100"
        );
        assert!(
            bench_spec::parse(&a.bench_words(shipped), Path::new("/scratch"))
                .is_ok_and(|spec| !spec.rate_limit),
            "the bench reads the word back"
        );
        let mut o = BTreeMap::new();
        o.insert("--rate-limit", "yes".to_string());
        assert!(Args::from_options(&o).is_err());
    }

    /// `--at-rest off,on --passes 2` is an A B B A set, each arm reproducible alone, and a plain run
    /// measures what ships: a sealed log.
    #[test]
    fn the_at_rest_arms_reach_the_bench_and_the_ledger() {
        let mut o = BTreeMap::new();
        o.insert("--at-rest", "off,on".to_string());
        o.insert("--passes", "2".to_string());
        let a = Args::from_options(&o).expect("parses");
        let plan = a.plan();
        let sealed: Vec<bool> = plan.iter().map(|arm| arm.at_rest).collect();
        assert_eq!(sealed, [false, true, true, false]);
        assert!(
            a.bench_words(plan[0]).ends_with("at_rest=off"),
            "{}",
            a.bench_words(plan[0])
        );
        assert!(!a.bench_words(plan[1]).contains("at_rest"));
        assert!(a.command_line(plan[0]).ends_with(" --at-rest off"));
        assert!(!a.command_line(plan[1]).contains("--at-rest"));
        let bare = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(bare.plan()[0].at_rest, "a bare run measures what ships");
        let spec = bench_spec::parse(&bare.bench_words(bare.plan()[0]), Path::new("/s"))
            .expect("the bench reads it");
        assert!(spec.at_rest, "and the bench is told so");
        let mut o = BTreeMap::new();
        o.insert("--at-rest", "sealed".to_string());
        assert!(
            Args::from_options(&o)
                .unwrap_err()
                .contains("`--at-rest` takes on or off")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_broker_process_arms_reach_the_bench_and_the_ledger() {
        use bench_spec::BrokerProcess;
        let mut o = BTreeMap::new();
        o.insert("--broker-process", "background,interactive".to_string());
        o.insert("--passes", "2".to_string());
        o.insert("--attach-sessions", "8,32".to_string());
        let a = Args::from_options(&o).expect("parses");
        let plan = a.plan();
        let processes: Vec<BrokerProcess> = plan.iter().map(|arm| arm.broker_process).collect();
        assert_eq!(
            processes,
            [
                BrokerProcess::Background,
                BrokerProcess::Interactive,
                BrokerProcess::Interactive,
                BrokerProcess::Background
            ],
            "one invocation is an A B B A set"
        );
        assert_eq!(
            a.bench_words(plan[0]),
            "attach=8,32 durability=ordered horizon_ms=100 broker_process=background"
        );
        assert!(
            a.command_line(plan[1])
                .ends_with(" --durability ordered --horizon-ms 100 --broker-process interactive"),
            "{}",
            a.command_line(plan[1])
        );
        for arm in &plan {
            let spec = bench_spec::parse(&a.bench_words(*arm), Path::new("/s"))
                .expect("the bench reads it");
            assert_eq!(
                spec.broker_process, arm.broker_process,
                "and the bench is told so"
            );
            // The entry's title names only the worlds `run/outside.rs` measures, and the arm.
            let title = heading(a.scenario, arm.broker_process);
            assert!(
                ["burst", "handoff", "ask", "cross-node"]
                    .iter()
                    .all(|world| !title.contains(world))
                    && title.contains(arm.broker_process.word()),
                "{title}"
            );
        }
        assert!(
            heading(bench_spec::Scenario::All, BrokerProcess::Inproc).contains("burst"),
            "the in-process arm's `all` measures every world"
        );
        let bare = Args::from_options(&BTreeMap::new()).expect("bare");
        assert_eq!(bare.plan()[0].broker_process, BrokerProcess::Inproc);
        assert!(!bare.bench_words(bare.plan()[0]).contains("broker_process"));
        assert!(
            !bare
                .command_line(bare.plan()[0])
                .contains("--broker-process")
        );
        let mut o = BTreeMap::new();
        o.insert("--broker-process", "launchd".to_string());
        assert!(
            Args::from_options(&o)
                .unwrap_err()
                .contains("`--broker-process` takes inproc, background or interactive")
        );
        let mut o = BTreeMap::new();
        o.insert("--broker-process", "background".to_string());
        o.insert("--scenario", "ask".to_string());
        assert!(
            Args::from_options(&o)
                .unwrap_err()
                .contains("the fabric bench refuses this run"),
            "the bench's own refusal comes before the build"
        );
        // F09: the attach world's backlog seeds the shipped broker's log; its log is sealed, as
        // every fabric root's broker serves it, so a plaintext arm is refused before the build.
        let mut o = BTreeMap::new();
        o.insert("--broker-process", "background".to_string());
        o.insert("--scenario", "attach".to_string());
        o.insert("--attach-sessions", "8,32".to_string());
        o.insert("--attach-backlog", "510000".to_string());
        let a = Args::from_options(&o).expect("the seeded arm parses");
        let arm = a.plan()[0];
        assert!(
            a.command_line(arm).contains(" --attach-backlog 510000")
                && a.bench_words(arm).contains("backlog=510000"),
            "{}",
            a.command_line(arm)
        );
        o.insert("--at-rest", "off".to_string());
        assert!(
            Args::from_options(&o)
                .unwrap_err()
                .contains("the fabric bench refuses this run")
        );
        o.remove("--at-rest");
        // A seed short of the cold reads' records is refused before the build, not after the
        // timed world: 40,000 for each of two populations seeds 80,000.
        o.insert("--attach-backlog", "40000".to_string());
        let why = Args::from_options(&o).expect_err("a seed short of the cold reads");
        assert!(
            why.contains("the fabric bench refuses this run")
                && why.contains(&format!(
                    "at least {}",
                    bench_spec::COLD_RECORDS.div_ceil(2)
                )),
            "{why}"
        );
    }

    #[test]
    fn the_bystanders_reach_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        for n in ["0", "64"] {
            let mut o = BTreeMap::new();
            o.insert("--bystanders", n.to_string());
            let a = Args::from_options(&o).expect("parses");
            assert_eq!(
                a.bench_words(shipped),
                format!("bystanders={n} durability=ordered horizon_ms=100")
            );
            assert!(
                a.command_line(shipped)
                    .contains(&format!(" --bystanders {n} ")),
                "{}",
                a.command_line(shipped)
            );
        }
        let a = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(!a.bench_words(shipped).contains("bystanders"));
        for bad in ["-1", "many", "999"] {
            let mut o = BTreeMap::new();
            o.insert("--bystanders", bad.to_string());
            assert!(Args::from_options(&o).is_err(), "{bad}");
        }
    }

    /// B09: the attach world's backlog reaches the bench's words and each
    /// entry's Run line, and the bench's own refusals reach the flag (500,000
    /// records at the default three attach sizes fill past the `ev` rule's bytes).
    #[test]
    fn the_attach_backlog_reaches_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        for n in ["0", "400000"] {
            let mut o = BTreeMap::new();
            o.insert("--attach-backlog", n.to_string());
            o.insert("--scenario", "attach".to_string());
            let a = Args::from_options(&o).expect("parses");
            assert_eq!(
                a.bench_words(shipped),
                format!("scenario=attach backlog={n} durability=ordered horizon_ms=100")
            );
            assert!(
                a.command_line(shipped)
                    .contains(&format!(" --scenario attach --attach-backlog {n} ")),
                "{}",
                a.command_line(shipped)
            );
        }
        let a = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(!a.bench_words(shipped).contains("backlog"));
        for (scenario, bad) in [
            ("attach", "-1"),
            ("attach", "many"),
            ("attach", "1000001"),
            ("attach", "500000"),
            ("pingpong", "10"),
        ] {
            let mut o = BTreeMap::new();
            o.insert("--attach-backlog", bad.to_string());
            o.insert("--scenario", scenario.to_string());
            assert!(Args::from_options(&o).is_err(), "{scenario} {bad}");
        }
    }

    /// C04: the ask world's asked prefill reaches the bench's words and each
    /// entry's Run line, and the bench's own refusals reach the flag (past the
    /// table's cap, or on a run that measures no ask).
    #[test]
    fn the_asked_prefill_reaches_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        for n in ["0", "1024"] {
            let mut o = BTreeMap::new();
            o.insert("--asked-prefill", n.to_string());
            o.insert("--scenario", "ask".to_string());
            let a = Args::from_options(&o).expect("parses");
            assert_eq!(
                a.bench_words(shipped),
                format!("scenario=ask asked_prefill={n} durability=ordered horizon_ms=100")
            );
            assert!(
                a.command_line(shipped)
                    .contains(&format!(" --scenario ask --asked-prefill {n} ")),
                "{}",
                a.command_line(shipped)
            );
        }
        let a = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(!a.bench_words(shipped).contains("asked_prefill"));
        for (scenario, bad) in [
            ("ask", "-1"),
            ("ask", "+5"),
            ("ask", "many"),
            ("ask", "1025"),
            ("attach", "10"),
        ] {
            let mut o = BTreeMap::new();
            o.insert("--asked-prefill", bad.to_string());
            o.insert("--scenario", scenario.to_string());
            assert!(Args::from_options(&o).is_err(), "{scenario} {bad}");
        }
    }

    /// C13: the messaging world's paced notes reach the bench's words and each entry's Run
    /// line, and the bench's own refusals reach the flag (past their cap, or on a run that
    /// measures no messaging).
    #[test]
    fn the_paced_notes_reach_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        for n in ["0", "60"] {
            let mut o = BTreeMap::new();
            o.insert("--paced-notes", n.to_string());
            o.insert("--scenario", "messaging".to_string());
            let a = Args::from_options(&o).expect("parses");
            assert_eq!(
                a.bench_words(shipped),
                format!("scenario=messaging paced_notes={n} durability=ordered horizon_ms=100")
            );
            assert!(
                a.command_line(shipped)
                    .contains(&format!(" --scenario messaging --paced-notes {n} ")),
                "{}",
                a.command_line(shipped)
            );
        }
        let a = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(!a.bench_words(shipped).contains("paced_notes"));
        for (scenario, bad) in [
            ("messaging", "-1"),
            ("messaging", "many"),
            ("messaging", "546"),
            ("pingpong", "10"),
        ] {
            let mut o = BTreeMap::new();
            o.insert("--paced-notes", bad.to_string());
            o.insert("--scenario", scenario.to_string());
            assert!(Args::from_options(&o).is_err(), "{scenario} {bad}");
        }
    }

    /// D02: `--tally` reaches the bench's words and each entry's Run line, a run without it
    /// names no tally (its worlds boot as before), and the bench's refusal of a ping-pong-only
    /// run reaches the flag.
    #[test]
    fn the_tally_reaches_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        let mut o = BTreeMap::new();
        o.insert("--tally", String::new());
        o.insert("--scenario", "attach".to_string());
        let a = Args::from_options(&o).expect("parses");
        let words = a.bench_words(shipped);
        assert!(
            bench_spec::parse(&words, Path::new("/s"))
                .expect("the bench reads them")
                .tally
        );
        let line = a.command_line(shipped);
        assert!(
            line.split_whitespace().any(|word| word == "--tally"),
            "{line}"
        );
        let bare = Args::from_options(&BTreeMap::new()).expect("bare");
        let words = bare.bench_words(shipped);
        assert!(
            !bench_spec::parse(&words, Path::new("/s"))
                .expect("bare words")
                .tally
        );
        assert!(
            !bare
                .command_line(shipped)
                .split_whitespace()
                .any(|word| word == "--tally")
        );
        let mut o = BTreeMap::new();
        o.insert("--tally", String::new());
        o.insert("--scenario", "pingpong".to_string());
        assert!(Args::from_options(&o).is_err(), "ping-pong counts anyway");
    }

    #[test]
    fn the_asks_and_the_mix_reach_the_bench_and_the_ledger() {
        let shipped = Arm::shipped();
        let mut o = BTreeMap::new();
        o.insert("--scenario", "ask".to_string());
        o.insert("--asks", "40".to_string());
        o.insert("--mix-every", "5".to_string());
        let a = Args::from_options(&o).expect("parses");
        let words = a.bench_words(shipped);
        assert_eq!(
            words,
            "scenario=ask asks=40 mix_every=5 durability=ordered horizon_ms=100"
        );
        let spec = bench_spec::parse(&words, Path::new("/s")).expect("the bench reads them");
        assert_eq!((spec.asks, spec.mix_every), (40, 5));
        let line = a.command_line(shipped);
        assert!(
            line.contains(" --scenario ask ") && line.contains(" --asks 40 --mix-every 5 "),
            "{line}"
        );
        let bare = Args::from_options(&BTreeMap::new()).expect("bare");
        assert!(!bare.bench_words(shipped).contains("asks"));
        for (flag, bad) in [("--asks", "0"), ("--asks", "x"), ("--mix-every", "0")] {
            let mut o = BTreeMap::new();
            o.insert(flag, bad.to_string());
            assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
        }
    }

    #[test]
    fn the_arms_run_in_order_and_alternate_by_pass() {
        use bench_spec::Tier;
        let arm = |tier, horizon_ms| Arm {
            tier,
            horizon_ms,
            at_rest: true,
            broker_process: bench_spec::BrokerProcess::Inproc,
        };
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
        assert_eq!(a.bench_words(o2), "durability=ordered horizon_ms=100");
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
            opt.bench_words(Arm::shipped()),
            "burst=150 durability=ordered horizon_ms=100"
        );

        let mut o = BTreeMap::new();
        o.insert("--profile", "shipped".to_string());
        let shipped = Args::from_options(&o).expect("parses");
        assert_eq!(shipped.profile, Profile::Shipped);
        assert_eq!(
            shipped.build_args(),
            [
                "--profile",
                "fabric-shipped",
                "-p",
                "aterm",
                "--bench",
                "fabric_bench",
                "--features",
                "dev-seams"
            ],
            "the shipped arm's seams come from the feature, not from debug assertions"
        );
        assert_eq!(
            shipped.command_line(Arm::shipped()),
            "targo --unverified verify perf fabric --profile shipped --durability ordered \
             --horizon-ms 100"
        );

        for bad in ["", "release", "opt-test", "fabric-shipped", "OPT"] {
            let mut o = BTreeMap::new();
            o.insert("--profile", bad.to_string());
            let why = Args::from_options(&o).expect_err(bad);
            assert!(why.contains("`dev`, `opt` or `shipped`"), "{bad}: {why}");
        }
    }
}
