// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf restore-e2e`: a crashed window's tabs back, end to end, for N sessions. The lane
//! builds aterm's `restore_bench` bench for its arm's profile ([`bench_build::prepare`]), runs it
//! once per arm (`clean`, what ships; `copy`, each region restored from a heap copy; `lookalike`,
//! that copy with the look-alike tail planted in every region, the seam's own copy and plant off
//! the readings), and writes each run as a dated entry in the verify state directory with its
//! provenance and the load beside it.
//!
//! # The restore ledger — what a crashed window's tabs cost to bring back
//!
//! A window killed without a word (SIGKILL, a jetsam kill, a crash) leaves its shells in the
//! keeper's custody, each beside its crash-restore region. The next launch says HELLO to the
//! keeper, is offered every orphaned session, and admits them one after another before its
//! first frame (`keeper_link::boot`: per offer `admit_recovered_master`, whose
//! `crash_restore::restore_offered` runs `aterm_restore::recover` over the region). This ledger
//! holds what that costs, from the successor's spawn to its tabs back. The runner is the
//! `restore-e2e` lane of `verify perf` (this module); the
//! measurement is `crates/aterm/benches/restore_bench.rs` (`tests/restore_bench/run.rs`, its
//! grammar in `spec.rs`), whose pure readings and tables the `restore_bench` test target checks.
//!
//! ```sh
//! targo --unverified verify perf restore-e2e --no-windows
//! # knobs (defaults shown):
//! targo --unverified verify perf restore-e2e --no-windows --profile dev --sessions 1,8,32 \
//!   --rounds 3 --lines 5000 --arm clean,copy,lookalike --lookalike-kib 6912
//! # the optimized build (cargo profile opt-test):
//! targo --unverified verify perf restore-e2e --no-windows --profile opt
//! # what ships and the look-alike arm, interleaved A B B A:
//! targo --unverified verify perf restore-e2e --no-windows --arm clean,lookalike --passes 2
//! ```
//!
//! `--sessions` rises, each at most 64; `--rounds` is at most 20, `--lines` at most 100,000, and
//! `--lookalike-kib` at most 8192, only when the plan holds the lookalike arm. The lane refuses
//! what the bench would before it builds. It opens no window and runs on macOS only.
//!
//! Each run writes `<date>-restore-e2e.md` (the tables under the run's provenance) and
//! `<date>-restore-e2e.json` (every round's raw readings and each size's summary) into the verify
//! state directory, `<target>/verify/.ubd/perf.noindex/restore/` ([`super::ledger_dir`]); a second
//! run on one day gets `-2`, `-3`, …. An entry is local state and never edited: a number a claim
//! rests on goes in the lab tracker with the command that produced it (`goals.py measure`). A list
//! of arms is one build, then one run and one entry per arm in the plan
//! (`--passes` repeats it, reversed on odd passes); each entry's **Run** line reproduces its arm
//! alone, and `--json-out` takes a plan of one run.
//!
//! ## What runs
//!
//! Each round is one scratch world (`tests/support/keeper_world.rs`, the live lane's own):
//!
//! 1. This build's `aterm keeper serve --no-relaunch`, and a headless instance A with
//!    `[keeper] enabled = true`.
//! 2. A runs `--sessions` sessions, each printing `--lines` lines of history
//!    (`seq -f 'line %g' 1 <lines>`) and a marker. The round waits, per session, for the marker,
//!    the prompt below it, `journal_tail=0` and `journal_ring_lag=0`, then for the keeper's
//!    `claimed=N` and `journals=N`.
//! 3. A is SIGKILLed by its own pid (never `pkill`) and reaped; the keeper says `orphaned=N`.
//! 4. B boots in the same world. Both keep a window's desk (layout, journals, restore pass)
//!    without its wait for glass (`ATERM_DEBUG_HEADLESS_RESTORE` under the world's own
//!    `ATERM_STATE_HOME`, `desk::Desk::LabHeadless`). A window places the sessions after the first
//!    once its first frame is on glass; a headless instance does it at its first park, so the
//!    placement reading leaves out a window's first present (`first_present_ms`).
//!
//! Every wait for an application event is a one-minute hang detector; nothing asserts a time.
//! The keeper's first exec of its fresh code copy has the shared first-exec bound, and a copy that
//! never showed it ran stays in place. Each round checks that its arm took, from B's `metrics`. A
//! debug or test build of B counts `restore_experiment_copies=`, `restore_experiment_plants=` and
//! `restore_experiment_refused=`:
//! clean needs all zero, copy one copy per offer, lookalike one copy and one plant per offer, and
//! no arm a refusal. B's seam counters must agree, and in the lookalike arm every recovered session
//! must have lost history to the plant. A failed copy or plant falls back to the original region,
//! so it cannot pass as an experimental timing. Completion needs exactly A's session IDs, each
//! restored and imported (membership, not tab order). A round that fails any of these fails the
//! run, and no entry is written.
//!
//! ## The readings
//!
//! On B's own clocks (`metrics`: `process_birth_to_rust_main_ms`; `startup_keeper_offers=
//! _hello_ms= _admit_ms= _admit_max_ms= _placed_ms= _imported_ms=`; and, with debug assertions,
//! which both profiles keep, `startup_keeper_seam_copies= _plants= _ms=`). The admission runs
//! before B's control socket exists, so nothing outside can see it.
//!
//! - **birth → HELLO answered**: B's process birth to the keeper's answer to its boot HELLO.
//! - **HELLO → every offer admitted**: the one-after-another admission; **slowest one offer** is
//!   its longest single admission.
//! - **HELLO → every session placed**: the restore pass that places every recovered session
//!   drained (where the history imports start): a person's tabs all there, less a window's first
//!   present.
//! - **HELLO → every import settled**: every session's history import done (`—` with none).
//! - **the seam's own work** (`startup_keeper_seam_ms`): the copy and lookalike arms' copying and
//!   planting. Every reading above leaves it out, since a really damaged region never pays it; the
//!   bench's readings below hold it, because it runs before B's control socket listens.
//!
//! B's recovery-count fence (`keeper_link::RecoveryCountAdmission`): until the keeper confirmed every
//! adopted session's new journal, B refuses every control request (`ERR recovery-count-custody
//! unconfirmed`), so no ID or history anchor escapes while an old journal is offerable. The fence
//! refuses `aterm ctl` clients (agents, scripts, the bench) and nothing else: B's restore pass,
//! history imports and glass do not read it. What it waits on: each adopted session's new journal
//! goes to the keeper as a COMPANION between two SYNCs on the keeper link
//! (`aterm_keeper::client::send_companion`), and the link's worker reading the keeper's answer to
//! the second confirms it (`ReadObserver`). On B's own clocks again (`startup_keeper_fence_cohort=
//! _sync_sent_ms= _sync_answer_max_ms= _fence_ms= _served_ms=`), from the HELLO and less the seam:
//!
//! - **HELLO → last confirming SYNC written**: the last of those second SYNCs, of the ones whose
//!   answers confirmed a journal.
//! - **slowest one SYNC → its answer read**: the longest any one journal waited on the keeper's
//!   answer and the worker's read of it.
//! - **HELLO → the fence lifted**: the last answer read, once every old journal authority (the
//!   cohort, one per offer with a region) has one.
//! - **HELLO → first request past it**: the first control request the fence let through.
//!
//! On the bench's clock, from just before B's spawn; everything after ready waits on that fence.
//!
//! - **spawn → ready**: B's ready signal, written once its control socket listens.
//! - **spawn → served past the fence**: the first request B answers, asked every 2 ms; a refused
//!   request reads only the fence's flag, so asking does not move the lift. **served's window**,
//!   from the last refused request (or ready) to the answer, is the most it can be late.
//!   **ready → served** is how long a client that dials at ready waits on the fence.
//! - **spawn → every `restore=` read**: the first sweep in which every session reads
//!   `restore=exact|screen|meta`.
//! - **HELLO → every `restore=` read**: that less B's birth → HELLO (so it also holds the gap from
//!   the bench's spawn call to B's birth).
//! - **spawn → every import in**: the first sweep in which every session's `lines` plus
//!   `history_lost=` reach A's count, with `journal=live journal_ring_lag=0`; a META rung has
//!   nothing to import.
//! - **longest sweep**: the polled readings' resolution (a sweep every 2 ms).
//!
//! Each round's row also gives `restore=`, `restore_why=`, the summed `history_lost=`, A's and
//! B's lines, the copies and plants, and the load just before B's spawn.
//!
//! ## The arms
//!
//! - `clean`: B restores each offered region as the keeper holds it. What ships.
//! - `copy`: B restores each region from a copy in its heap
//!   (`ATERM_DEBUG_CRASH_RESTORE_LOOKALIKE_KIB=0`, debug and test builds): against clean, what a
//!   heap copy rather than the mapped region changes, the lookalike arm's residue.
//! - `lookalike`: that copy with `aterm_restore::plant::lookalike_tail` planted
//!   (`…_LOOKALIKE_KIB=<--lookalike-kib>`): the newest history chunk's payload damaged and frame
//!   look-alikes past the ring's commit, up to its lap: `benches/resync_walk.rs`'s `lookalike-tail`
//!   case, the damage the resync's bound was built for, in every region at once. It is the
//!   costliest tail in an optimized build (`docs/DESIGN-crash-restore-2026-10-20.md` §3: 5.8 ms
//!   against 3.6 ms for a random tail of the full ring), not in a dev one (`resync_walk` under
//!   `--profile test`, 2026-10-27, three runs each: 38.7 ms over a random tail against 8.8 ms over
//!   this one). The lane plants no random tail.
//!
//! The keeper's region is only read; the copy and the plant are the seam's own work, off B's clocks.
//!
//! ## How to compare two runs
//!
//! Same machine, same day, one build, the arms interleaved (`--passes 2`: A B B A), and the load
//! beside every number: the lane waits for a quiet machine before it starts (`--admit wait`, the
//! default), and each round records `vm.loadavg` before B. A `dev` and an `opt` entry measure
//! different code generation and do not compare.

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
            raw.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| raw.parse::<u32>().ok())
                .flatten()
                .ok_or_else(|| format!("`{flag}` needs a positive integer, got `{raw}`"))
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
                "--profile" => {
                    a.profile = Profile::parse(raw)?;
                    if a.profile == Profile::Shipped {
                        return Err(
                            "`--profile shipped` is the fabric lane's: the restore bench drives \
                             seams debug assertions compile in"
                                .to_string(),
                        );
                    }
                }
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
            bench_spec::parse(&a.bench_words(arm), Path::new("/scratch"))
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

    /// The bench's measurement knobs for `arm`.
    pub(crate) fn bench_words(&self, arm: Arm) -> String {
        let mut words = Vec::new();
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
    let ledger_dir = super::ledger_dir("restore");
    std::fs::create_dir_all(&ledger_dir)
        .map_err(|e| format!("could not make {}: {e}", ledger_dir.display()))?;
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
        built,
        bench_build::Entry::Timed,
    )?;
    let read = |name: &str| {
        let path = scratch.path().join(name);
        crate::read_regular_text(&path)
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
        protocol: "crates/aterm-verify/src/perf/restore_e2e.rs",
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
    crate::write_fresh(&md_path, format!("{header}\n{tables}\n"))
        .map_err(|e| format!("could not write {}: {e}", md_path.display()))?;
    crate::write_fresh(&json_path, &json)
        .map_err(|e| format!("could not write {}: {e}", json_path.display()))?;
    eprintln!(
        "ledger entry written: {} (+ {})",
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
#[path = "restore_e2e_tests.rs"]
mod tests;
