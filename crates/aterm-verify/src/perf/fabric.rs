// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `fabric` lane: what aterm's use of the astream bus costs, MEASURED and
//! never judged.
//!
//! The measurement itself is Trust code in the package it measures:
//! `crates/aterm/tests/fabric_bench`, a test target that mounts the fabric
//! worlds' harness and boots the real stack — astream's broker opened guarded
//! (Strict, as aterm opens it) in the bench process, the headless `aterm` and
//! the `aterm link serve` bridge that `aterm` supervises. Its measuring entry
//! is `#[ignore]`d and inert unless `FABRIC_BENCH` names a run; this lane is
//! what names one:
//!
//! ```text
//! targo --unverified test -q -p aterm --test fabric_bench -- --exact measure --ignored --nocapture
//! ```
//!
//! with `FABRIC_BENCH="out=<scratch> samples=… burst=… pingpong=… attach=…
//! rounds=…"`. The bench writes `fabric.json` (every raw value beside its
//! summary) and `fabric.md` (the tables); the lane prints the tables, writes both
//! into the append-only ledger `docs/measured/fabric/` with the run's
//! provenance, and copies the JSON to `--json-out` when given.
//!
//! `--pingpong-rounds N` sizes the ping-pong arm (A posts, B replies on
//! receipt, back to back): its round trip, each reply's wait for the commit of
//! the delivery it answers, and the share of the window the bridge's loop spent
//! blocked on broker replies, which the bridge counts itself (its wait-tally
//! development seam, `aterm_link::wait_tally`, set in that arm's world alone).
//!
//! `--durability relaxed` opens the bench's broker at astream's Relaxed tier
//! (the same guard, no fsync before an ack): the ceiling arm, never what aterm
//! runs, and its entry says so above its tables.
//!
//! WHY NO FLOOR. Every message costs Strict appends, and a Strict append is an
//! fsync — `F_FULLFSYNC` on macOS — whose price is the disk's: an absolute
//! bound would fail a correct build on a slower machine and pass a regression
//! on a faster one. A before/after claim is two runs of this lane on one
//! machine, compared; the ledger keeps them.
//!
//! TWO ARMS, `--profile dev|opt`, BOTH TEST BUILDS. The harness reaches the
//! `aterm` binary through `CARGO_BIN_EXE_aterm` and the bridge through the
//! `ATERM_FABRIC_COMMAND` development seam, which a build without debug
//! assertions compiles out, and a `--release` test build of a crate that calls
//! other crates' debug-only test seams does not compile (AGENTS.md). So `dev`
//! (the default, and every entry before the opt arm) is `targo test`'s own
//! profile, where most crates on the path — aterm-link, astream-broker,
//! aterm-gui — are opt-level 0 and its numbers overstate the CPU part of every
//! cost; and `opt` is the root manifest's `[profile.opt-test]`: release code
//! generation with debug assertions on (the arm's table, and why it does not
//! inherit `dev`, is the bench's `fabric_bench/profile.rs`). Before measuring,
//! the lane reads cargo's plan for the arm and refuses an opt build with any
//! crate on the measured path below opt-level 3 ([`units`]), and holds the
//! rustflags cargo appends beside the plan to setting none of what it resolved
//! ([`flags`]: the caller's environment sources are removed, the config files'
//! are read). It builds, takes the `aterm` cargo reports and that file's
//! identity, and after the run refuses a report of any other path, or of that
//! path holding another file at the bench's start or end. Every entry names its
//! arm, what the plan said and every rustflag beside it; compare like with like.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio};

use super::arena::{self, Halt};
use super::{LaneRun, workspace_root};
use crate::perf::driver::CargoDriver;

/// THE BENCH'S OWN GRAMMAR for a run, mounted from the bench (std-only) the way
/// `crate::toolchain` is shared with atpkg: the lane refuses exactly what the
/// bench would — before the build, as a usage error, never as a red lane after
/// one — and no bound (`BURST_MAX`, `ATTACH_MAX`, the rising list) is kept in
/// two places to drift.
#[path = "../../../aterm/tests/fabric_bench/spec.rs"]
#[expect(
    dead_code,
    reason = "the lane validates with `parse`; the knobs a run reads back and `from_env` are the \
              bench's"
)]
mod bench_spec;

/// THE BENCH'S OWN TABLE of arms, mounted the same way: the cargo profile each
/// builds with, where its binaries land, and the opt-level it promises.
#[path = "../../../aterm/tests/fabric_bench/profile.rs"]
mod bench_profile;

mod flags;
mod units;

use bench_profile::Profile;

/// The lane's options, parsed, and checked by the bench's own grammar. `None`
/// leaves the bench's default (`crates/aterm/tests/fabric_bench/spec.rs`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    pub samples: Option<u32>,
    pub burst: Option<u32>,
    /// The ping-pong arm's measured round trips (`--pingpong-rounds`).
    pub pingpong: Option<u32>,
    pub attach: Option<Vec<u32>>,
    pub rounds: Option<u32>,
    /// The broker tier (`--durability strict|relaxed`), checked by the bench's
    /// own word parser; `None` is the bench's default, Strict.
    pub durability: Option<bench_spec::Tier>,
    pub json_out: Option<PathBuf>,
    /// The arm measured (`--profile`), dev unless named.
    pub profile: Profile,
}

fn positive(flag: &str, raw: &str) -> Result<u32, String> {
    match raw.parse::<u32>() {
        Ok(v) if v > 0 => Ok(v),
        _ => Err(format!("`{flag}` needs a positive integer, got `{raw}`")),
    }
}

impl Args {
    /// Parse the options the fabric lane reads — `super::main` hands it only
    /// those (`super::options_for`), so any other flag here is one the table
    /// gives this lane and this parser forgot.
    ///
    /// # Errors
    ///
    /// A value that is not a positive integer, a run the bench's grammar
    /// refuses (a burst or population past what one instance carries, a
    /// population list that does not rise), or a flag with no parser.
    pub(crate) fn from_options(options: &BTreeMap<&'static str, String>) -> Result<Args, String> {
        let mut a = Args::default();
        for (flag, raw) in options {
            match *flag {
                "--latency-samples" => a.samples = Some(positive(flag, raw)?),
                "--burst" => a.burst = Some(positive(flag, raw)?),
                "--pingpong-rounds" => a.pingpong = Some(positive(flag, raw)?),
                "--attach-rounds" => a.rounds = Some(positive(flag, raw)?),
                "--attach-sessions" => {
                    a.attach = Some(raw.split(',').map(|n| positive(flag, n)).collect::<Result<
                        Vec<u32>,
                        String,
                    >>(
                    )?);
                }
                "--durability" => {
                    a.durability = Some(bench_spec::Tier::parse(raw).map_err(|_| {
                        format!("`--durability` is `strict` or `relaxed`, not `{raw}`")
                    })?);
                }
                "--json-out" => a.json_out = Some(PathBuf::from(raw)),
                "--profile" => a.profile = Profile::parse(raw)?,
                // The run's switch, which `super::main` reads; it names every
                // lane, so every lane's parser is handed it.
                "--no-windows" => {}
                other => {
                    return Err(format!(
                        "internal: `{other}` has no parser in the fabric lane"
                    ));
                }
            }
        }
        bench_spec::parse(&a.bench_words(Path::new("/scratch")))
            .map_err(|why| format!("the fabric bench refuses this run: {why}"))?;
        Ok(a)
    }

    /// The `FABRIC_BENCH` words for a run writing into `out`. Pure
    /// (unit-tested).
    pub(crate) fn bench_words(&self, out: &Path) -> String {
        let mut words = vec![format!("out={}", out.display())];
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
        if let Some(tier) = self.durability {
            words.push(format!("durability={}", tier.word()));
        }
        words.join(" ")
    }

    /// The `verify perf fabric …` line that reproduces this run, for the
    /// ledger.
    pub(crate) fn command_line(&self) -> String {
        let mut line = String::from("targo --unverified verify perf fabric");
        // The default arm is left unsaid, so a dev entry reads exactly as every
        // entry before the opt arm did.
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
        if let Some(tier) = self.durability {
            line.push_str(&format!(" --durability {}", tier.word()));
        }
        line
    }

    /// The cargo arguments that name this arm's build of the bench: the arm's
    /// profile, the package, the test target. The plan and the run both take
    /// them, so the build the lane checks is the build it runs. Pure
    /// (unit-tested).
    pub(crate) fn build_args(&self) -> Vec<&'static str> {
        let mut args = Vec::new();
        if let Some(profile) = self.profile.cargo_profile() {
            args.extend(["--profile", profile]);
        }
        args.extend(["-p", "aterm", "--test", "fabric_bench"]);
        args
    }
}

/// The variable the bench reads its run from.
const BENCH_ENV: &str = "FABRIC_BENCH";

/// The bench's progress lines start with this; the lane echoes them live and
/// keeps the rest of the build's chatter for a failure message.
const PROGRESS: &str = "fabric bench:";

/// How many trailing output lines a failure quotes.
const TAIL: usize = 60;

/// The lane.
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
    // THE FLAGS BESIDE THE PROFILE: the caller's environment sources are
    // removed from every targo below; the config files' are read and held to
    // the plan ([`flags`]).
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
    eprintln!("  {build}");
    let load_before = load_average();
    // THE BINARY THIS ARM BUILT, as cargo reports it, and its identity right
    // after the build: the bench must report launching exactly that file, at
    // its start and at its end.
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
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-fabric-")
        .tempdir()
        .map_err(|e| format!("could not make a scratch directory: {e}"))?;
    let words = args.bench_words(scratch.path());
    let mut test_args = vec!["-q"];
    test_args.extend(args.build_args());
    test_args.extend(["--", "--exact", "measure", "--ignored", "--nocapture"]);
    eprintln!(
        "  $ {BENCH_ENV}=\"{words}\" {}",
        targo.display("test", &test_args)
    );
    let mut child = targo
        .command("test")
        .args(test_args)
        .env(BENCH_ENV, &words)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run targo: {e}"))?;
    // stdout carries libtest's lines and the tables the bench also writes to
    // its report file: read on a thread so neither pipe can fill and stall the
    // other, and kept only for a failure message.
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
    if let Some(why) = another_build(&report, &built, &built_identity) {
        return Err(Halt::Fail(format!(
            "{why}: these numbers are not the `{}` arm's build, and no ledger entry was written",
            profile.name()
        )));
    }
    // The entry's JSON carries what the lane read about the build, beside the
    // bench's own words for the arm.
    if let Some(fields) = report.as_object_mut() {
        fields.insert("lane_build".to_string(), build.clone().into());
    }
    let json = aterm_json::to_string(&report)
        .map_err(|e| format!("the bench's report does not serialize: {e}"))?;
    let load = format!(
        "{load_before} before the build, {} after the run",
        load_average()
    );
    eprintln!("{tables}");

    let date = super::trend::utc_date();
    let header = Provenance {
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
        command: &args.command_line(),
        build: &build,
        thermal: &arena::thermal(),
        cpus: &cpus(),
        load: &load,
    }
    .render();
    let (md_path, json_path) = ledger_paths(&ledger_dir, &date);
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
    Ok(format!("ledger entry {}", md_path.display()))
}

/// Every targo this lane runs: the run's toolchain ([`CargoDriver`]), in the
/// workspace, with no environment source of rustflags ([`flags`]), and led by
/// the build-cache `--config` atpkg's shim hands a plain `targo` here
/// ([`crate::main_build_dir`]) when it would hand one.
///
/// ONE CACHE WITH A SHELL BUILD. Without that `--config` the lane compiled into
/// `target/<profile>/deps` while `targo --unverified test …` from a shell (and
/// `verify`'s own test builds) compiled the same units into the shim's build
/// directory: two full copies of one build, 3.7 GiB for the dev arm and 727 MiB
/// for the opt arm. With it, the lane's units are the shell's; the binaries
/// still land in `target/<profile dir>/`.
struct Targo<'a> {
    driver: &'a CargoDriver,
    build_dir: Option<String>,
}

impl<'a> Targo<'a> {
    fn new(driver: &'a CargoDriver) -> Self {
        Targo {
            driver,
            build_dir: crate::main_build_dir(&crate::EnvSnapshot::capture(), &workspace_root()),
        }
    }

    /// The arguments before the caller's: the cache `--config`, the lane flag,
    /// the verb.
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
    /// stderr's tail.
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

/// Cargo's plan for this arm's build of the bench (`--unit-graph`, which
/// compiles nothing), read down to the units on the measured path.
fn plan(targo: &Targo<'_>, args: &Args) -> Result<units::Audit, Halt> {
    let mut plan_args = args.build_args();
    plan_args.extend(["--unit-graph", "-Z", "unstable-options"]);
    let graph = targo.output("test", &plan_args, "the build plan")?;
    units::Audit::of(&graph)
        .map_err(|why| Halt::Fail(format!("the build plan does not read: {why}")))
}

/// The rustflags cargo's config files hold (`targo config get` of the `build`
/// and `target` tables; a table not set is none).
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
/// says it wrote that `aterm`.
fn build_bench(targo: &Targo<'_>, args: &Args) -> Result<PathBuf, Halt> {
    let mut build_args = vec!["-q"];
    build_args.extend(args.build_args());
    build_args.extend(["--no-run", "--message-format", "json-render-diagnostics"]);
    eprintln!("  $ {}", targo.display("test", &build_args));
    let messages = targo.output("test", &build_args, "the bench's build")?;
    built_binary(&messages, "aterm").map_err(|why| Halt::Fail(format!("the bench's build: {why}")))
}

/// The executable cargo reports for the binary target `name`, out of a
/// build's `--message-format json` lines. Pure (unit-tested).
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
/// or the same path holding another file at the run's start or end — or
/// `None`. Pure (unit-tested).
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
/// day already wrote one: the ledger is append-only.
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
///
/// RECORDED BECAUSE IT MOVES THE NUMBERS: every message here waits on fsyncs,
/// and on a host shared with builds and other agents an `F_FULLFSYNC` flushes
/// their writes too. The entry says how busy the machine was, so two entries
/// are compared knowing it; the bench's own yardstick is the in-run
/// correction.
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
/// says (Apple silicon), how many are performance cores. A load of 12 is a
/// quiet 18-core machine and a queue on its 6 performance cores, and every
/// fabric hop is a thread waiting for one.
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
        format!(
            "<!-- SPDX-License-Identifier: Apache-2.0 -->\n\
             <!-- Copyright 2026 Andrew Yates -->\n\
             \n\
             # Fabric: post -> deliver, burst, ping-pong, attach — {date}\n\
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
        assert_eq!(a.samples, Some(50));
        assert_eq!(a.attach, Some(vec![1, 4, 16]));
        assert_eq!(
            a.bench_words(Path::new("/s")),
            "out=/s samples=50 attach=1,4,16"
        );
        assert_eq!(
            a.command_line(),
            "targo --unverified verify perf fabric --latency-samples 50 --attach-sessions 1,4,16"
        );
        // The ceiling arm: the tier reaches the bench's words and the ledger's
        // reproducing line.
        o.insert("--durability", "relaxed".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.durability, Some(bench_spec::Tier::Relaxed));
        assert_eq!(
            a.bench_words(Path::new("/s")),
            "out=/s samples=50 attach=1,4,16 durability=relaxed"
        );
        assert!(
            a.command_line().ends_with(" --durability relaxed"),
            "{}",
            a.command_line()
        );
        // The ping-pong arm's rounds reach the bench's words and the ledger's
        // reproducing line, under the bench's own word for them.
        o.insert("--pingpong-rounds", "40".to_string());
        let a = Args::from_options(&o).expect("parses");
        assert_eq!(a.pingpong, Some(40));
        assert_eq!(
            a.bench_words(Path::new("/s")),
            "out=/s samples=50 pingpong=40 attach=1,4,16 durability=relaxed"
        );
        assert!(
            a.command_line().contains(" --pingpong-rounds 40 "),
            "{}",
            a.command_line()
        );
        // No option: the bench's own defaults.
        assert_eq!(Args::default().bench_words(Path::new("/s")), "out=/s");
        // Refused here, before any build: what does not parse, and what the
        // bench's own grammar refuses (past one instance's capacity, a list
        // that does not rise).
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
            ("--durability", "ordered"),
            ("--durability", "Relaxed"),
        ] {
            let mut o = BTreeMap::new();
            o.insert(flag, bad.to_string());
            assert!(Args::from_options(&o).is_err(), "{flag} {bad}");
        }
        // At the bench's bounds exactly: taken.
        let mut o = BTreeMap::new();
        o.insert("--burst", bench_spec::BURST_MAX.to_string());
        o.insert("--attach-sessions", format!("1,{}", bench_spec::ATTACH_MAX));
        assert!(Args::from_options(&o).is_ok());
        // A flag this lane is handed but has no arm for is an internal error,
        // never silently dropped.
        let mut o = BTreeMap::new();
        o.insert("--warmup", "3".to_string());
        assert!(Args::from_options(&o).is_err());
    }

    #[test]
    fn the_profile_option_picks_the_arm_its_build_and_its_command_line() {
        // Unnamed: the dev arm, built and reproduced exactly as every entry
        // before the opt arm was.
        let dev = Args::from_options(&BTreeMap::new()).expect("parses");
        assert_eq!(dev.profile, Profile::Dev);
        assert_eq!(dev.build_args(), ["-p", "aterm", "--test", "fabric_bench"]);
        assert_eq!(dev.command_line(), "targo --unverified verify perf fabric");
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
                "--test",
                "fabric_bench"
            ]
        );
        // The ledger's command line reproduces the arm; the bench's words
        // never carry it (the bench names its arm from the binary it runs).
        assert_eq!(
            opt.command_line(),
            "targo --unverified verify perf fabric --profile opt --burst 150"
        );
        assert_eq!(opt.bench_words(Path::new("/s")), "out=/s burst=150");

        for bad in ["", "release", "opt-test", "OPT"] {
            let mut o = BTreeMap::new();
            o.insert("--profile", bad.to_string());
            let why = Args::from_options(&o).expect_err(bad);
            assert!(why.contains("`dev` or `opt`"), "{bad}: {why}");
        }
    }

    #[test]
    fn the_built_aterm_is_the_binary_cargo_reports() {
        // The shape of the lines `--no-run --message-format json` printed for
        // the opt arm on 2026-10-17 (fields trimmed): the test executable sits
        // in the shim's build directory, the binary in the target directory.
        let messages = [
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm"},"executable":null,"fresh":true}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"filenames":["/w/target/opt-test/aterm"],"executable":"/w/target/opt-test/aterm","fresh":false}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"fabric_bench"},"executable":"/c/.cargo-build/opt-test/deps/fabric_bench-7b2c","fresh":false}"#,
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
        // The dev arm's binary, or any other path.
        let why =
            another_build(&report("/w/target/debug/aterm", id, id), built, id).expect("refused");
        assert!(why.contains("/w/target/debug/aterm"), "{why}");
        // The right path, rewritten before the run or during it.
        let other = "84000000 bytes, modified 1792221999.000000001";
        for (start, end, moment) in [(other, id, "start"), (id, other, "end")] {
            let why = another_build(&report(path, start, end), built, id).expect("refused");
            assert!(why.contains(moment) && why.contains(other), "{why}");
        }
        // A bench that reported no identity measured no known binary.
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
