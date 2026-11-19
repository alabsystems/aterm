// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the first submit after the GPU went quiet costs, and on which threads: the quiet-commit
//! grid (`benches/quiet/plan.rs`). The 2026-11-17 profile of a resting window's reading frame
//! fits its ~4.2 ms submit burst at 2.94 ms plus 6.9 us per MiB the driver takes back, so about
//! 1.2 ms tracks the freed block and about 3 ms grows with how long the GPU sat idle. This
//! separates the two: block 1 is the cost against the quiet for the renderer's overlay-only
//! present (arm P: into the headless renderer's offscreen target and waited on, not the windowed
//! present, which a windowed step answers) and four hand-built shapes, block 2 the blit and
//! compute shapes, block 3 which level holds the idle state (the same queue, another queue,
//! another process; every level at 500 ms, the other process at 100 ms too), block 4 a
//! residency restore (512 MiB of ballast).
//!
//! `QUIET_COMMIT_REPS=8 targo --unverified bench -p aterm-gpu --features compute --bench
//! quiet_commit > quiet.txt 2>&1` runs it; without `--features compute` the compute arm and
//! keep are left out and a `SKIP` line says so. Its output goes to a file, never a terminal
//! (it refuses a terminal stdout) and never through `tee`: a terminal's redraw of each line is
//! another process's GPU work inside the next quiet. It opens no window and reaches no window
//! server, but it shares the GPU with whatever else draws: it measures other processes' GPU
//! time over each quiet and acts on none of them. Every repetition runs each take of the plan
//! once in its own shuffled order. Configuration is by environment alone (cargo bench hands a
//! harness-less bench `--bench`):
//! - `QUIET_COMMIT_REPS`: repetitions, default 8;
//! - `QUIET_COMMIT_ARMS`: a comma list of arm names to keep (`p,clear,dot,two_cb,empty_cb,
//!   blit,compute`), default all;
//! - `QUIET_COMMIT_SEED`: the shuffle's seed, decimal or `0x` hex, default the plan's `SEED`;
//! - `QUIET_COMMIT_FOREIGN_MAX_DUTY`: the highest foreign duty a take after a quiet may have
//!   for the decisions to take it: other processes' GPU time from its `QuietStart` mark to its
//!   second read of every GPU client (over its quiet, and at `completed` through the take as
//!   well), divided by the stretch between those two reads, a fraction (0 or more), default
//!   the plan's `FOREIGN_MAX_DUTY`, 0.10; a back-to-back take has no quiet, and the bound
//!   leaves it alone;
//! - `QUIET_COMMIT_IOKIT_AT`: where the quiet-end-side IOKit reads and purgeable walk are made,
//!   `quiet_end` (default), `completed`, or `alternate` (even repetitions one, odd the other).
//!   A decision run uses `quiet_end`: the `foreign_split` control judges those takes alone,
//!   whose foreign read ends before the submit, so a run at `completed` alone is void;
//! - `QUIET_COMMIT_JSON`: a file for the run object, one JSON object per take as each take
//!   ends, and the controls object (a relative path is taken from the package's directory,
//!   where cargo runs a bench);
//! - `QUIET_COMMIT_GIT_SHA`: the commit to record; unset, `git rev-parse HEAD` is run in the
//!   package's directory (`CARGO_MANIFEST_DIR`, read at run time);
//! - `QUIET_COMMIT_SIZE`: the hand-built arms' target, `WxH`, default 1760x1024;
//! - `QUIET_COMMIT_PX`: arm P's font size in device px, default 18.
//!
//! The sampler thread (`quiet-sampler`) reads this process's VM ledgers every 10 ms throughout,
//! and every thread's CPU every 1 ms from each take's submit through a last cut at or after
//! 40 ms; each thread whose CPU grew since the cut before has its dispatch queue read, and each
//! thread's instructions, cycles and energy are read at the window's two ends. A window whose
//! last cut fell short is not read, and the decisions leave its take out. Every GPU client's
//! time is read once the take's ballast and keeper child are made (its `QuietStart` mark, at or
//! after the quiet's start), at the quiet's end (or just after the completion, by
//! `QUIET_COMMIT_IOKIT_AT`) and at the settle, which gives this process's own GPU time and
//! other processes' (its keeper child aside), and each take's foreign duty: the foreign time
//! from the first read to the second over the stretch between them. It prints a `HEAD` line,
//! a `TAKE` line per take with a `RELEASE` line per footprint step in its quiet and its
//! teardown, `ARM` lines per take shape over every read window (`set=all`) and over the
//! admitted takes (`set=quiet`), an `EXCLUDED` line, then for each placement a `SPLIT` line
//! per stratum of at least two admitted takes after a quiet whose foreign duty, other-thread
//! CPU and graphics rise were read (a take shape at that placement, its re-takes apart from
//! its held takes: its duty span, its other-thread CPU median and its own slope on the duty,
//! recorded and not judged) and a `POOLED` line (the strata's pooled slope times the duty
//! range of their takes, `judged=true` at `quiet_end` alone), and a `VALID` or `VOID` line per
//! validity control over the admitted takes, `foreign_split` (the `quiet_end` placement's
//! `POOLED` effect, at most 250 µs either way) among them: a run with any `VOID` is recorded,
//! never decided on.

#[cfg(target_os = "macos")]
use aterm_gpu::{QuietArm, QuietKeep, QuietTake};

#[cfg(target_os = "macos")]
#[path = "quiet/plan.rs"]
mod quiet_plan;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    if std::env::var("QUIET_COMMIT_ROLE").as_deref() == Ok("keeper") {
        return grid::keeper();
    }
    match grid::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("quiet_commit: could not run: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(target_os = "macos")]
mod grid {
    use std::collections::HashMap;
    use std::io::{IsTerminal as _, Read as _, Write as _};
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use aterm_gpu::{QuietCommit, QuietHost, QuietMark, QuietSetup, quiet_commit_probe};
    use aterm_sysprobe::{GpuClients, self_cpu, self_vm, thread_counts};
    use aterm_uds::process::continuous_ns;

    use super::quiet_plan::{
        self, Excluded, Field, Placement, PlacementMode, Planned, ReleasePhase, Scope, TakeRecord,
        ThreadRole, ThreadSample, VmPoint, WindowCut, mib,
    };
    use super::{QuietArm, QuietKeep, QuietTake};

    /// The thread that runs the probe: the caller of every take.
    const PROBE_THREAD: &str = "quiet-probe";
    /// The sampler's thread.
    const SAMPLER_THREAD: &str = "quiet-sampler";
    /// How long each take's CPU window runs from its submit, at least.
    const WINDOW: Duration = Duration::from_millis(40);
    /// The window's cut cadence.
    const CUT_EVERY: Duration = Duration::from_millis(1);
    /// The VM ledgers' cadence.
    const VM_EVERY: Duration = Duration::from_millis(10);
    /// A wait for something that must happen (the sampler's window, a keeper's exit).
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// The keeper child (`QUIET_COMMIT_ROLE=keeper`): a 1x1 clear every
    /// `QUIET_COMMIT_KEEP_PERIOD_MS` on its own device until its stdin reaches EOF, then one
    /// stdout line per clear: when it completed, on the boot's continuous clock in ns, which
    /// the parent reads on the same clock.
    pub(super) fn keeper() -> ExitCode {
        let period = match std::env::var("QUIET_COMMIT_KEEP_PERIOD_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            Some(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                eprintln!("quiet_commit keeper: QUIET_COMMIT_KEEP_PERIOD_MS is not a period");
                return ExitCode::from(2);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&stop);
        let reader = std::thread::Builder::new()
            .name("quiet-keeper-stdin".to_owned())
            .spawn(move || {
                let mut buf = [0u8; 64];
                let mut stdin = std::io::stdin();
                while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
                seen.store(true, Ordering::Release);
            });
        if let Err(e) = reader {
            eprintln!("quiet_commit keeper: no stdin reader: {e}");
            return ExitCode::from(2);
        }
        let mut done = Vec::new();
        let ran = aterm_gpu::quiet_keeper(period, &|| stop.load(Ordering::Acquire), &mut || {
            done.push(continuous_ns().unwrap_or(0));
        });
        let mut out = std::io::stdout().lock();
        for ns in &done {
            let _ = writeln!(out, "{ns}");
        }
        let _ = out.flush();
        match ran {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("quiet_commit keeper: {e}");
                ExitCode::from(2)
            }
        }
    }

    pub(super) fn run() -> Result<(), String> {
        if std::io::stdout().is_terminal() {
            return Err(
                "stdout is a terminal: run with stdout to a file (`> quiet.txt`), not \
                        a terminal or tee, whose redraws are another process's GPU work"
                    .to_owned(),
            );
        }
        let reps: usize = env_or("QUIET_COMMIT_REPS", 8)?;
        let seed = match std::env::var("QUIET_COMMIT_SEED") {
            Ok(v) => parse_seed(&v).ok_or_else(|| format!("QUIET_COMMIT_SEED={v:?}"))?,
            Err(_) => quiet_plan::SEED,
        };
        let foreign_max_duty = match std::env::var("QUIET_COMMIT_FOREIGN_MAX_DUTY") {
            Ok(v) => v
                .parse::<f64>()
                .ok()
                .filter(|d| d.is_finite() && *d >= 0.0)
                .ok_or_else(|| {
                    format!(
                        "QUIET_COMMIT_FOREIGN_MAX_DUTY={v:?} is not a duty: a fraction, 0 or more"
                    )
                })?,
            Err(_) => quiet_plan::FOREIGN_MAX_DUTY,
        };
        let placement = match std::env::var("QUIET_COMMIT_IOKIT_AT") {
            Ok(v) => PlacementMode::parse(&v).ok_or_else(|| {
                format!("QUIET_COMMIT_IOKIT_AT={v:?} is not quiet_end, completed or alternate")
            })?,
            Err(_) => PlacementMode::Every(Placement::QuietEnd),
        };
        let size = match std::env::var("QUIET_COMMIT_SIZE") {
            Ok(s) => {
                let (w, h) = s
                    .split_once('x')
                    .ok_or_else(|| format!("QUIET_COMMIT_SIZE={s:?} is not WxH"))?;
                (
                    w.parse().map_err(|e| format!("width {w:?}: {e}"))?,
                    h.parse().map_err(|e| format!("height {h:?}: {e}"))?,
                )
            }
            Err(_) => (1760, 1024),
        };
        let px: f32 = match std::env::var("QUIET_COMMIT_PX") {
            Ok(v) => v
                .parse()
                .map_err(|e| format!("QUIET_COMMIT_PX={v:?}: {e}"))?,
            Err(_) => 18.0,
        };
        let arms: Option<Vec<QuietArm>> = match std::env::var("QUIET_COMMIT_ARMS") {
            Ok(list) => Some(
                list.split(',')
                    .map(|a| {
                        QuietArm::parse(a.trim())
                            .ok_or_else(|| format!("QUIET_COMMIT_ARMS names no arm {a:?}"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Err(_) => None,
        };
        let compute = cfg!(feature = "compute");
        if !compute {
            println!(
                "SKIP arm=compute keep=other_queue_compute reason=built_without_features_compute"
            );
        }
        let admit = |t: &QuietTake| {
            arms.as_ref().is_none_or(|a| a.contains(&t.arm))
                && (compute
                    || (t.arm != QuietArm::Compute
                        && !matches!(t.keep, Some(QuietKeep::OtherQueueCompute(_)))))
        };
        let planned = quiet_plan::plan(reps, seed, admit);
        if planned.is_empty() {
            return Err("the plan admits no take".to_owned());
        }
        let mut json = match std::env::var_os("QUIET_COMMIT_JSON") {
            Some(path) => Some(
                std::fs::File::create(&path)
                    .map_err(|e| format!("QUIET_COMMIT_JSON={}: {e}", path.display()))?,
            ),
            None => None,
        };
        let quiet_s: f64 = planned.iter().map(|p| p.take.quiet.as_secs_f64()).sum();
        let mut facts = vec![
            ("reps", Field::Int(reps as u64)),
            ("takes", Field::Int(planned.len() as u64)),
            ("quiet_s", Field::Float(quiet_s, 1)),
            ("size", Field::Text(format!("{}x{}", size.0, size.1))),
            ("px", Field::Float(f64::from(px), 1)),
            ("pid", Field::Int(u64::from(std::process::id()))),
            ("compute", Field::Bool(compute)),
            ("seed", Field::Text(format!("{seed:#018x}"))),
            ("iokit_at", Field::Text(placement.name().to_owned())),
            (
                "foreign_max_duty",
                Field::Float(foreign_max_duty, quiet_plan::DUTY_PLACES),
            ),
        ];
        facts.extend(run_facts());
        println!("{}", quiet_plan::fields_line("HEAD", &facts));
        let mut run_object = vec![("kind", Field::Text("run".to_owned()))];
        run_object.extend(facts);
        write_json(&mut json, &quiet_plan::object(&run_object));
        let epoch = Instant::now();
        let shared = Arc::new(Shared::default());
        let sampler = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name(SAMPLER_THREAD.to_owned())
                .spawn(move || sample(&shared, epoch))
                .map_err(|e| format!("no sampler thread: {e}"))?
        };
        let host = Host {
            epoch,
            pid: std::process::id(),
            shared: Arc::clone(&shared),
            planned: planned.clone(),
            placement,
            keeper: None,
            json,
            current: Readings::default(),
            last_vm: None,
            records: Vec::with_capacity(planned.len()),
        };
        let takes: Vec<QuietTake> = planned.iter().map(|p| p.take).collect();
        let probe = std::thread::Builder::new()
            .name(PROBE_THREAD.to_owned())
            .spawn(move || {
                let mut host = host;
                let ran = quiet_commit_probe(QuietSetup { size, px }, &takes, &mut host);
                let _ = host.keeper_stop_if_any();
                (ran.map(drop), host.records, host.json)
            })
            .map_err(|e| format!("no probe thread: {e}"))?;
        let joined = probe.join();
        shared.stop();
        let _ = sampler.join();
        let (ran, records, mut json) =
            joined.map_err(|_| "the probe thread panicked".to_owned())?;
        for scope in [Scope::All, Scope::Quiet(foreign_max_duty)] {
            for line in quiet_plan::arm_lines(&records, scope) {
                println!("{line}");
            }
        }
        let excluded = Excluded::of(&records, foreign_max_duty);
        println!("{}", excluded.line(foreign_max_duty));
        let splits =
            Placement::ALL.map(|at| quiet_plan::foreign_split(&records, foreign_max_duty, at));
        for split in &splits {
            for row in &split.strata {
                println!("{}", row.line());
            }
            println!("{}", split.line());
        }
        let controls = quiet_plan::controls(&records, foreign_max_duty);
        for control in &controls {
            println!("{}", control.line());
        }
        write_json(
            &mut json,
            &quiet_plan::controls_json(&controls, &splits, &excluded, foreign_max_duty),
        );
        ran
    }

    fn env_or(name: &str, default: usize) -> Result<usize, String> {
        match std::env::var(name) {
            Ok(v) => v.parse().map_err(|e| format!("{name}={v:?}: {e}")),
            Err(_) => Ok(default),
        }
    }

    /// A seed in decimal or `0x` hex.
    fn parse_seed(v: &str) -> Option<u64> {
        match v.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16).ok(),
            None => v.parse().ok(),
        }
    }

    /// One line into the JSON file, which a failed write closes.
    fn write_json(json: &mut Option<std::fs::File>, object: &str) {
        if let Some(file) = json.as_mut()
            && let Err(e) = writeln!(file, "{object}")
        {
            eprintln!("quiet_commit: QUIET_COMMIT_JSON write failed ({e}); no more lines");
            *json = None;
        }
    }

    /// What the run records about the machine and the build, each read once before the plan.
    fn run_facts() -> Vec<(&'static str, Field)> {
        let text = |v: Option<String>| v.map_or(Field::Null, Field::Text);
        let words = aterm_sysprobe::machine_words();
        let gpu = aterm_sysprobe::gpu();
        let driver = aterm_sysprobe::gpu_driver();
        let power = aterm_sysprobe::situation::power_sources();
        let thermal = aterm_sysprobe::process_thermal();
        let (sha, dirty) = git_state();
        vec![
            ("machine", text(aterm_sysprobe::machine_model())),
            ("cpu", text(words.cpu)),
            ("os", text(words.os)),
            ("os_build", text(aterm_sysprobe::os_build())),
            ("gpu", text(gpu.as_ref().and_then(|g| g.name.clone()))),
            (
                "gpu_cores",
                gpu.as_ref()
                    .and_then(|g| g.cores)
                    .map_or(Field::Null, |c| Field::Int(u64::from(c))),
            ),
            (
                "gpu_driver",
                text(driver.as_ref().and_then(|d| d.bundle.clone())),
            ),
            (
                "gpu_driver_version",
                text(driver.as_ref().and_then(|d| d.version.clone())),
            ),
            (
                "power",
                text(power.map(|p| format!("{:?}", p.source).to_lowercase())),
            ),
            (
                "battery_pct",
                power
                    .and_then(|p| p.battery)
                    .map_or(Field::Null, |b| Field::Int(u64::from(b.pct))),
            ),
            (
                "low_power",
                thermal
                    .and_then(|t| t.low_power)
                    .map_or(Field::Null, Field::Bool),
            ),
            (
                "thermal",
                text(thermal.map(|t| format!("{:?}", t.state).to_lowercase())),
            ),
            ("git", text(sha)),
            ("git_dirty", dirty.map_or(Field::Null, Field::Bool)),
        ]
    }

    /// The commit this runs from and whether its tracked files differ from it:
    /// `QUIET_COMMIT_GIT_SHA` where set (dirtiness then unread), else `git` in the package's
    /// directory, read at run time; `None` where neither answers.
    fn git_state() -> (Option<String>, Option<bool>) {
        if let Ok(sha) = std::env::var("QUIET_COMMIT_GIT_SHA") {
            return (Some(sha), None);
        }
        let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
            return (None, None);
        };
        let git = |args: &[&str]| {
            aterm_uds::spawnfd::command("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        };
        let sha = git(&["rev-parse", "HEAD"]);
        let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
        (sha, dirty)
    }

    /// A finished window: its cuts, the reads that failed, and the end it had to reach.
    struct WindowDone {
        cuts: Vec<WindowCut>,
        fails: u32,
        end_ns: u64,
    }

    /// What the sampler and the probe thread share.
    #[derive(Default)]
    struct Shared {
        state: Mutex<SamplerState>,
        wake: Condvar,
    }

    #[derive(Default)]
    struct SamplerState {
        stop: bool,
        /// A requested window's end and each thread's CPU at its first cut, taken by the
        /// sampler when it starts it.
        window: Option<(Instant, HashMap<u64, u64>)>,
        /// The last window, finished and not yet collected.
        done: Option<WindowDone>,
        /// VM readings since the last drain.
        vm: Vec<VmPoint>,
    }

    impl Shared {
        fn lock(&self) -> std::sync::MutexGuard<'_, SamplerState> {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn stop(&self) {
            self.lock().stop = true;
            self.wake.notify_all();
        }

        fn start_window(&self, until: Instant, first: HashMap<u64, u64>) {
            let mut s = self.lock();
            s.window = Some((until, first));
            s.done = None;
            drop(s);
            self.wake.notify_all();
        }

        /// The finished window, waiting for it under the hang detector.
        fn collect_window(&self) -> Result<WindowDone, String> {
            let deadline = Instant::now() + HANG_DETECTOR;
            let mut s = self.lock();
            loop {
                if let Some(done) = s.done.take() {
                    return Ok(done);
                }
                let left = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(|| "the sampler's window did not finish in a minute".to_owned())?;
                s = self
                    .wake
                    .wait_timeout(s, left)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        }

        fn drain_vm(&self) -> Vec<VmPoint> {
            std::mem::take(&mut self.lock().vm)
        }
    }

    /// Each thread's CPU at `cut`, ns, where read.
    fn cpu_of(cut: &self_cpu::Cut) -> HashMap<u64, u64> {
        cut.threads
            .iter()
            .filter_map(|t| Some((t.id, t.cpu?.ns)))
            .collect()
    }

    /// The sampler: VM ledgers every [`VM_EVERY`] throughout; when a window is asked for, every
    /// thread's CPU every [`CUT_EVERY`], and a last cut that starts at or after the window's
    /// end, then each live thread's counts. A thread whose CPU grew since the cut before (or
    /// that the cut before did not hold) has its dispatch queue read.
    fn sample(shared: &Shared, epoch: Instant) {
        let pid = std::process::id();
        let mut next_vm = Instant::now();
        let vm_tick = |next_vm: &mut Instant| {
            if Instant::now() >= *next_vm {
                if let Some(p) = vm_point(epoch) {
                    shared.lock().vm.push(p);
                }
                *next_vm = (*next_vm + VM_EVERY).max(Instant::now());
            }
        };
        loop {
            let window = {
                let mut s = shared.lock();
                if s.stop {
                    return;
                }
                s.window.take()
            };
            let Some((until, first)) = window else {
                vm_tick(&mut next_vm);
                let s = shared.lock();
                if !s.stop && s.window.is_none() {
                    let left = next_vm.saturating_duration_since(Instant::now());
                    drop(
                        shared
                            .wake
                            .wait_timeout(s, left)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
                continue;
            };
            let mut prev = first;
            let mut cuts = Vec::new();
            let mut fails = 0u32;
            let mut next = Instant::now();
            loop {
                vm_tick(&mut next_vm);
                let at_end = Instant::now() >= until;
                let mut grew = |id: u64, cpu: Option<self_cpu::CpuCounter>| {
                    cpu.is_some_and(|c| prev.get(&id).is_none_or(|&p| c.ns > p))
                };
                match self_cpu::read_asking(epoch, &mut grew) {
                    Ok(cut) => {
                        prev = cpu_of(&cut);
                        cuts.push(cut);
                    }
                    Err(_) => fails += 1,
                }
                if at_end {
                    break;
                }
                next = (next + CUT_EVERY).max(Instant::now()).min(until);
                sleep_until(next);
            }
            let counts = cuts.last().map(|c| counts_of(pid, c)).unwrap_or_default();
            let last = cuts.len().saturating_sub(1);
            let series = cuts
                .into_iter()
                .enumerate()
                .map(|(i, c)| window_cut(c, (i == last).then_some(&counts)))
                .collect();
            let mut s = shared.lock();
            s.done = Some(WindowDone {
                cuts: series,
                fails,
                end_ns: ns_at(epoch, until),
            });
            drop(s);
            shared.wake.notify_all();
        }
    }

    fn sleep_until(at: Instant) {
        if let Some(left) = at.checked_duration_since(Instant::now()) {
            std::thread::sleep(left);
        }
    }

    fn vm_point(epoch: Instant) -> Option<VmPoint> {
        let vm = self_vm::read().ok()?;
        Some(VmPoint {
            at_ns: ns_since(epoch),
            phys: vm.phys_footprint,
            gfx: Some(vm.ledger_tag_graphics_footprint),
        })
    }

    fn ns_since(epoch: Instant) -> u64 {
        ns_at(epoch, Instant::now())
    }

    fn ns_at(epoch: Instant, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(epoch).as_nanos()).unwrap_or(u64::MAX)
    }

    /// Each thread of `cut`'s counts, every performance level together.
    fn counts_of(pid: u32, cut: &self_cpu::Cut) -> HashMap<u64, thread_counts::LevelCounts> {
        cut.threads
            .iter()
            .filter_map(|t| Some((t.id, thread_counts::read(pid, t.id)?.total())))
            .collect()
    }

    /// `cut` as the window's arithmetic reads it, with `counts` where they were read.
    fn window_cut(
        cut: self_cpu::Cut,
        counts: Option<&HashMap<u64, thread_counts::LevelCounts>>,
    ) -> WindowCut {
        WindowCut {
            at_ns: cut.begin_ns,
            process_ns: cut.process_before.ns,
            complete: cut.coverage.unread == 0 && !cut.coverage.capped,
            threads: cut
                .threads
                .into_iter()
                .map(|t| {
                    let c = counts.and_then(|m| m.get(&t.id));
                    ThreadSample {
                        id: t.id,
                        role: match t.name.as_deref() {
                            Some(PROBE_THREAD) => ThreadRole::Caller,
                            Some(SAMPLER_THREAD) => ThreadRole::Sampler,
                            _ => ThreadRole::Other,
                        },
                        cpu_ns: t.cpu.map(|c| c.ns),
                        user_ns: t.split.map(|s| s.user_ns),
                        system_ns: t.split.map(|s| s.system_ns),
                        instructions: c.map(|c| c.instructions),
                        cycles: c.map(|c| c.cycles),
                        energy_nj: c.map(|c| c.energy_nj),
                        name: t.name,
                        queue: t.queue,
                    }
                })
                .collect(),
        }
    }

    /// The heavy reads: the GPU's use and every GPU client's time (IOKit), and the purgeable
    /// walk of this process's map.
    #[derive(Default)]
    struct Heavy {
        device_pct: Option<u8>,
        /// When the walk of every GPU client started, ns since the run's epoch.
        clients_from_ns: u64,
        clients: Option<GpuClients>,
        vm: Option<self_vm::VmReading>,
    }

    fn heavy(epoch: Instant) -> Heavy {
        let device_pct = aterm_sysprobe::gpu().and_then(|g| g.device_pct);
        let clients_from_ns = ns_since(epoch);
        let clients = aterm_sysprobe::gpu_clients();
        Heavy {
            device_pct,
            clients_from_ns,
            clients,
            vm: self_vm::read_purgeable().ok(),
        }
    }

    /// What the keeper child reported on its exit.
    struct KeeperReport {
        ok: bool,
        /// Each clear's completion, on the continuous clock.
        done: Vec<u64>,
        /// Its spawn, on the same clock.
        spawned: Option<u64>,
    }

    /// One take's readings, gathered at its marks.
    #[derive(Default)]
    struct Readings {
        /// Every client at the quiet's start.
        start_clients: Option<GpuClients>,
        /// When that walk returned, ns since the run's epoch.
        start_clients_ns: Option<u64>,
        quiet_vm: Option<self_vm::VmReading>,
        quiet_prev: Option<VmPoint>,
        quiet_points: Vec<VmPoint>,
        /// At the take's placement.
        mid: Option<Heavy>,
        first: Option<WindowCut>,
        submit_clock: Option<u64>,
        completed_ns: Option<u64>,
        window: Option<Result<WindowDone, String>>,
        peak_phys: Option<u64>,
        peak_gfx: Option<i64>,
        settled_vm: Option<self_vm::VmReading>,
        settled: Option<Heavy>,
        keeper_pid: Option<u32>,
        keeper: Option<KeeperReport>,
    }

    struct Keeper {
        child: aterm_uds::spawnfd::Child,
        spawned: Option<u64>,
    }

    struct Host {
        epoch: Instant,
        pid: u32,
        shared: Arc<Shared>,
        planned: Vec<Planned>,
        placement: PlacementMode,
        keeper: Option<Keeper>,
        json: Option<std::fs::File>,
        current: Readings,
        last_vm: Option<VmPoint>,
        records: Vec<TakeRecord>,
    }

    impl Host {
        fn placement_of(&self, take: usize) -> Placement {
            self.placement
                .of(self.planned.get(take).map_or(0, |p| p.rep))
        }

        /// This process's own accumulated GPU time in `clients`.
        fn own(&self, clients: Option<&GpuClients>) -> Option<u64> {
            clients.map(|c| {
                c.rows
                    .iter()
                    .filter(|r| r.pid == self.pid)
                    .map(|r| r.gpu_ns)
                    .sum()
            })
        }

        fn keeper_stop_if_any(&mut self) -> Result<bool, String> {
            if self.keeper.is_some() {
                self.keeper_stop()
            } else {
                Ok(true)
            }
        }
    }

    impl QuietHost for Host {
        fn mark(&mut self, take: usize, mark: QuietMark) {
            match mark {
                QuietMark::QuietStart => {
                    self.current.start_clients = aterm_sysprobe::gpu_clients();
                    self.current.start_clients_ns = Some(ns_since(self.epoch));
                }
                QuietMark::QuietEnd => {
                    self.current.quiet_vm = self_vm::read().ok();
                    self.current.quiet_prev = self.last_vm;
                    let points = self.shared.drain_vm();
                    self.last_vm = points.last().copied().or(self.last_vm);
                    self.current.quiet_points = points;
                    if self.placement_of(take) == Placement::QuietEnd {
                        self.current.mid = Some(heavy(self.epoch));
                    }
                }
                QuietMark::Submit => {
                    self.current.submit_clock = continuous_ns();
                    let (first, cpu) = match self_cpu::read(self.epoch) {
                        Ok(cut) => {
                            let cpu = cpu_of(&cut);
                            let counts = counts_of(self.pid, &cut);
                            (Some(window_cut(cut, Some(&counts))), cpu)
                        }
                        Err(_) => (None, HashMap::new()),
                    };
                    self.current.first = first;
                    self.shared.start_window(Instant::now() + WINDOW, cpu);
                }
                QuietMark::Committed => {}
                QuietMark::Completed => {
                    self.current.completed_ns = Some(ns_since(self.epoch));
                    if self.placement_of(take) == Placement::Completed {
                        self.current.mid = Some(heavy(self.epoch));
                    }
                }
                QuietMark::Settled => {
                    self.current.window = Some(self.shared.collect_window());
                    let points = self.shared.drain_vm();
                    self.last_vm = points.last().copied().or(self.last_vm);
                    let settled_vm = self_vm::read().ok();
                    let phys = points
                        .iter()
                        .map(|p| p.phys)
                        .chain(settled_vm.map(|v| v.phys_footprint));
                    self.current.peak_phys = phys.max();
                    let gfx = points
                        .iter()
                        .filter_map(|p| p.gfx)
                        .chain(settled_vm.map(|v| v.ledger_tag_graphics_footprint));
                    self.current.peak_gfx = gfx.max();
                    self.current.settled_vm = settled_vm;
                    self.current.settled = Some(heavy(self.epoch));
                }
            }
        }

        fn took(&mut self, index: usize, commit: &QuietCommit) {
            let got = std::mem::take(&mut self.current);
            let Some(planned) = self.planned.get(index).copied() else {
                return;
            };
            let teardown_prev = self.last_vm;
            let teardown = self.shared.drain_vm();
            self.last_vm = teardown.last().copied().or(self.last_vm);
            let us = |d: Duration| d.as_secs_f64() * 1e6;
            let ledger_mib = |b: i64| b as f64 / 1_048_576.0;
            let mut r = TakeRecord::new(index, planned.rep, planned.block, planned.take);
            r.prev = index
                .checked_sub(1)
                .and_then(|p| Some((p, self.planned.get(p)?.take.arm)));
            r.iokit_at = self.placement_of(index);
            r.at_ns = Some(ns_at(self.epoch, commit.began));
            let quiet_from_ns = ns_at(self.epoch, commit.quiet_from);
            r.quiet_from_ns = Some(quiet_from_ns);
            r.idle_us = Some(us(commit.idle));
            r.queue_idle_us = commit.queue_idle.map(us);
            r.submit_us = Some(us(commit.submit));
            r.complete_us = Some(us(commit.complete));
            r.gpu_us = commit.gpu.map(us);
            r.keeps = commit.keeps;
            r.keep_ok = commit.keep_ok;
            r.last_keep_to_submit_us = commit.last_keep_to_submit.map(us);
            match got.window {
                Some(Ok(done)) => {
                    r.cuts = done.cuts.len();
                    r.cut_fails = done.fails;
                    r.last_cut_after_done_us = done
                        .cuts
                        .last()
                        .zip(got.completed_ns)
                        .map(|(c, d)| (c.at_ns as f64 - d as f64) / 1e3);
                    r.window = got
                        .first
                        .as_ref()
                        .and_then(|first| quiet_plan::window_cpu(first, &done.cuts, done.end_ns));
                }
                Some(Err(e)) => eprintln!("quiet_commit: take {index}: {e}"),
                None => {}
            }
            r.fp_quiet = got.quiet_vm.map(|v| mib(v.phys_footprint));
            r.gfx_fp = got
                .quiet_vm
                .map(|v| ledger_mib(v.ledger_tag_graphics_footprint));
            r.gfx_nofp = got
                .quiet_vm
                .map(|v| ledger_mib(v.ledger_tag_graphics_nofootprint));
            r.purg_vol = got
                .quiet_vm
                .map(|v| ledger_mib(v.ledger_purgeable_volatile));
            let resident = |h: Option<&Heavy>| {
                h.and_then(|h| h.vm)
                    .and_then(|v| v.purgeable)
                    .map(|p| mib(p.resident))
            };
            r.purg_vol_res = resident(got.mid.as_ref());
            r.purg_vol_res_settled = resident(got.settled.as_ref());
            r.fp_peak = got.peak_phys.map(mib);
            r.gfx_fp_peak = got.peak_gfx.map(ledger_mib);
            r.fp_settled = got.settled_vm.map(|v| mib(v.phys_footprint));
            r.device_pct = got.mid.as_ref().and_then(|h| h.device_pct);
            r.device_pct_settled = got.settled.as_ref().and_then(|h| h.device_pct);
            let start = got.start_clients.as_ref();
            let mid = got.mid.as_ref().and_then(|h| h.clients.as_ref());
            let settled = got.settled.as_ref().and_then(|h| h.clients.as_ref());
            let grew = |a: Option<u64>, b: Option<u64>| b?.checked_sub(a?);
            r.gpu_quiet_ns = grew(self.own(start), self.own(mid));
            r.gpu_ns = grew(self.own(mid), self.own(settled));
            let mut mine = vec![self.pid];
            mine.extend(got.keeper_pid);
            r.foreign = start
                .zip(mid)
                .map(|(a, b)| quiet_plan::foreign(a, b, &mine));
            r.foreign_span_ns = got
                .start_clients_ns
                .zip(got.mid.as_ref().map(|h| h.clients_from_ns))
                .and_then(|(a, b)| b.checked_sub(a));
            r.foreign_take = mid
                .zip(settled)
                .map(|(a, b)| quiet_plan::foreign(a, b, &mine));
            if let Some(k) = &got.keeper {
                r.keeper_commits = Some(k.done.len() as u64);
                r.keeper_first_commit_ms = k
                    .done
                    .first()
                    .zip(k.spawned)
                    .map(|(d, s)| (*d as f64 - s as f64) / 1e6);
                r.last_keep_to_submit_us = got.submit_clock.and_then(|submit| {
                    let last = k.done.iter().copied().filter(|d| *d <= submit).max()?;
                    Some((submit - last) as f64 / 1e3)
                });
                r.keep_ok = Some(k.ok);
            }
            r.releases = quiet_plan::releases(
                got.quiet_prev,
                &got.quiet_points,
                quiet_from_ns,
                ReleasePhase::Quiet,
            );
            r.releases.extend(quiet_plan::releases(
                teardown_prev,
                &teardown,
                got.completed_ns.unwrap_or(quiet_from_ns),
                ReleasePhase::Teardown,
            ));
            println!("{}", r.line());
            for step in &r.releases {
                println!("{}", step.line(index));
            }
            write_json(&mut self.json, &r.json());
            self.records.push(r);
        }

        fn keeper_start(&mut self, period: Duration) -> Result<(), String> {
            if self.keeper.is_some() {
                return Err("a keeper child is already running".to_owned());
            }
            let exe = std::env::current_exe().map_err(|e| format!("this bench's path: {e}"))?;
            let spawned = continuous_ns();
            let child = aterm_uds::spawnfd::command(exe)
                .env("QUIET_COMMIT_ROLE", "keeper")
                .env(
                    "QUIET_COMMIT_KEEP_PERIOD_MS",
                    period.as_millis().to_string(),
                )
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::inherit())
                .spawn()
                .map_err(|e| format!("the keeper child did not start: {e}"))?;
            self.current.keeper_pid = Some(child.id());
            self.keeper = Some(Keeper { child, spawned });
            Ok(())
        }

        fn keeper_stop(&mut self) -> Result<bool, String> {
            let Keeper { mut child, spawned } = self
                .keeper
                .take()
                .ok_or_else(|| "no keeper child".to_owned())?;
            drop(child.stdin.take());
            let deadline = Instant::now() + HANG_DETECTOR;
            let ok = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(
                            "the keeper child did not exit within a minute of its stdin closing"
                                .to_owned(),
                        );
                    }
                    Err(e) => return Err(format!("the keeper child's status: {e}")),
                }
            };
            // It wrote its lines before it exited: one per clear, at most 21 bytes each. Over
            // the plan's 5 s quiet, the take and its settle, the 100 ms keeper made 50-51
            // clears in the grid's first full run, about 1 KiB at most, well inside the pipe's
            // buffer, so its writes did not wait on this read; the pipe holds them and then
            // reads EOF.
            let mut out = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut out);
            }
            let done = out
                .lines()
                .filter_map(|l| l.trim().parse::<u64>().ok())
                .collect();
            self.current.keeper = Some(KeeperReport { ok, done, spawned });
            Ok(ok)
        }
    }
}

/// The Metal arm exists only on macOS.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("quiet_commit: the Metal arm is macOS's; nothing to measure here");
}
