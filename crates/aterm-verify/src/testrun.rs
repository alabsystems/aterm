// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TEST BINARIES, SEVERAL AT A TIME.
//!
//! WHY. cargo runs test binaries one at a time, even under `--no-fail-fast`, so
//! a workspace's few hundred binaries, most of them under a second each, would
//! be a serial walk on the run's critical path. This module runs several at
//! once. They start longest first by the last run's times ([`start_order`]),
//! up to `--test-jobs` at once ([`DEFAULT_JOBS`]) and more than [`FLOOR_JOBS`]
//! only while the machine's load is under its cores ([`Limits::by_load`]), each
//! with the whole pinned `RUST_TEST_THREADS`, as cargo's own run hands it.
//!
//! THE PROCESS IS CARGO'S, NOT A GUESS AT IT. A test binary is not just a path:
//! cargo starts it in its package's root with an environment of its own —
//! `CARGO` (under targo, targo itself), `CARGO_MANIFEST_DIR`, the whole
//! `CARGO_PKG_*` family, the dynamic-library search path, every
//! `cargo:rustc-env` its build script printed, targo's own
//! `TRUST_TARGO_FRONTEND` — and tests in this tree read several of those at
//! run time (`CARGO` in aterm-gui, aterm-forge and the conformance
//! support). Rebuilding that from the compile's JSON would be a copy of cargo
//! that drifts. So cargo is asked: the stage runs the very `targo test --tests`
//! it always ran, with one addition — `--config
//! target.'cfg(all())'.runner=[<this binary>, --record-test-binary, <dir>]` —
//! and cargo, instead of executing each test binary, executes this binary with
//! the test binary's argv, in its directory, with its environment.
//! [`record_main`] writes those three facts down and exits 0, so cargo walks
//! every binary in a few seconds and runs nothing; the run then starts the
//! recorded invocations itself.
//!
//! WHAT ELSE IT READS. The compile child (`--no-run`) is asked for
//! `--message-format=json-render-diagnostics`: its diagnostics print exactly as
//! before, and each `compiler-artifact` line names a test executable and its
//! target's kind and name — which, with the recorded `CARGO_PKG_NAME`, is
//! cargo's own re-run spec (`-p atpkg --test atpkg_suite`, `-p aterm-gui
//! --lib`), the id every failed test is known by and rerun with
//! ([`crate::findings::test_findings`]). The JSON lines themselves are
//! removed from what the ladder shows.
//!
//! THE LOG IS CARGO'S SHAPE. Each binary's output is captured on its own and
//! printed in cargo's order — its `Running` header, its bytes, and, when it
//! failed, cargo's `error: test failed, to rerun pass` line naming its spec —
//! so the ladder reads exactly as a serial run's would, and every reader of
//! that log ([`crate::libtest::failed_tests`],
//! [`crate::libtest::environment_refusals`]) reads it unchanged. The header is
//! written at the head of each binary's own log before it starts
//! ([`exec::Cmd::preamble`]), so a wall-clock kill names the binary and the
//! test still running in it, per binary ([`crate::libtest::note`]).
//!
//! NEVER LESS THAN CARGO. Anything that does not add up — the recording pass
//! failed, cargo announced a binary the recorder did not see (or the reverse),
//! a recorded binary the compile did not report — and the stage runs the plain
//! serial `targo test --tests` child it always ran, saying why in one line. A
//! recording pass in which the recorder never ran (a runner the caller's own
//! cargo config names outranks this one) WAS that serial run, and is judged as
//! one. No retry anywhere: a red binary is decided once, as cargo decided it.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::exec::{self, Cmd, Run};
use crate::ladder::Report;

/// The flag cargo's runner config hands this binary: `aterm-verify
/// --record-test-binary <dir> <test binary> <its args…>` ([`record_main`]).
pub const RECORD_FLAG: &str = "--record-test-binary";

/// The most test binaries that run at once unless `--test-jobs` says
/// otherwise — a CAP, not a level: past [`FLOOR_JOBS`] the pool starts one more
/// only while the machine's one-minute load is under its cores
/// ([`Limits::by_load`]).
///
/// Eight: two at a time already took the run to about 60 % of cargo's serial
/// walk, and with the long poles started first the whole tree's test run still
/// held the run's critical path at two (`docs/measured/test-pool-2026-09-30.md`
/// has the measurements).
pub const DEFAULT_JOBS: u32 = 8;

/// How many test binaries the pool always lets run at once when it has them,
/// whatever the load, so a busy machine still runs two at a time.
/// `--test-jobs 1` still means one at a time: the floor is never above the cap.
pub const FLOOR_JOBS: usize = 2;

/// THE MEMORY CAP: the most test binaries `ram` bytes hold at once beside the
/// workspace builds `plan` can run during the test run
/// ([`crate::plan::builds_held`] less [`crate::plan::builds_beside_tests`]),
/// never under [`FLOOR_JOBS`]; an unread host gets the floor. Past it, a small
/// host swaps and the tests that wait on sockets fail on its clock.
#[must_use]
pub fn memory_jobs(ram: Option<u64>, plan: &[crate::plan::StageSpec]) -> usize {
    ram.map_or(FLOOR_JOBS, |ram| {
        let beside = u64::try_from(crate::plan::builds_beside_tests(plan)).unwrap_or(u64::MAX);
        let holds = crate::plan::builds_held(ram).saturating_sub(beside);
        usize::try_from(holds).unwrap_or(usize::MAX).max(FLOOR_JOBS)
    })
}

// NO BINARY RUNS ALONE. Every live suite scratches under its own pid-keyed
// world (private sockets, state and HOME), so two binaries booting headless
// instances never meet, and every binary runs in the pool.

// ---------------------------------------------------------------------------
// The record: what cargo would have started.
// ---------------------------------------------------------------------------

const RECORD_MAGIC: &[u8] = b"aterm-verify test record 1\0";

/// One test binary as cargo would have started it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The directory cargo starts it in: its package's root.
    pub cwd: PathBuf,
    /// The test binary and the arguments cargo hands it (libtest's `--skip`s).
    pub argv: Vec<OsString>,
    /// Its whole environment.
    pub env: Vec<(OsString, OsString)>,
}

#[cfg(unix)]
fn bytes_of(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn bytes_of(s: &OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn os_of(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt as _;
    OsStr::from_bytes(b).to_os_string()
}

#[cfg(not(unix))]
fn os_of(b: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(b).into_owned())
}

impl Record {
    /// This process, as cargo started it: `argv` is the test binary's.
    ///
    /// # Errors
    /// When the working directory cannot be read.
    pub fn of_this_process(argv: Vec<OsString>) -> std::io::Result<Self> {
        Ok(Self {
            cwd: std::env::current_dir()?,
            argv,
            env: std::env::vars_os().collect(),
        })
    }

    /// The value of `key` in the recorded environment.
    #[must_use]
    pub fn var(&self, key: &str) -> Option<&OsStr> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_os_str())
    }

    /// The test binary.
    #[must_use]
    pub fn exe(&self) -> &Path {
        self.argv.first().map_or(Path::new(""), Path::new)
    }

    /// NUL-separated tokens after a magic line: `cwd`, then `arg` per argument,
    /// then `env <key> <value>` per variable. Unix strings hold no NUL, so no
    /// value can forge a token.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = RECORD_MAGIC.to_vec();
        let mut token = |t: &[u8]| {
            out.extend_from_slice(t);
            out.push(0);
        };
        token(b"cwd");
        token(&bytes_of(self.cwd.as_os_str()));
        for a in &self.argv {
            token(b"arg");
            token(&bytes_of(a));
        }
        for (k, v) in &self.env {
            token(b"env");
            token(&bytes_of(k));
            token(&bytes_of(v));
        }
        out
    }

    /// [`Record::encode`] read back; `None` for anything else, including a
    /// file cut short.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let body = bytes.strip_prefix(RECORD_MAGIC)?;
        let body = body.strip_suffix(b"\0")?;
        let mut it = body.split(|b| *b == 0);
        let (mut cwd, mut argv, mut env) = (None, Vec::new(), Vec::new());
        while let Some(tag) = it.next() {
            match tag {
                b"cwd" => cwd = Some(PathBuf::from(os_of(it.next()?))),
                b"arg" => argv.push(os_of(it.next()?)),
                b"env" => {
                    let k = os_of(it.next()?);
                    env.push((k, os_of(it.next()?)));
                }
                _ => return None,
            }
        }
        (!argv.is_empty()).then_some(())?;
        Some(Self {
            cwd: cwd?,
            argv,
            env,
        })
    }
}

/// `aterm-verify --record-test-binary <dir> <test binary> <args…>`: write this
/// process's invocation to the next free `<dir>/NNNNNN.rec` and exit 0, so
/// cargo goes on to the next binary having run nothing. The file is created
/// owner-only (it holds the environment) and never overwritten; cargo runs one
/// binary at a time, and a name taken by another writer is skipped all the
/// same. Returns the exit code.
#[must_use]
pub fn record_main(args: &[OsString]) -> i32 {
    let [dir, argv @ ..] = args else {
        eprintln!("aterm-verify: {RECORD_FLAG} needs a directory and a test binary");
        return 2;
    };
    if argv.is_empty() {
        eprintln!("aterm-verify: {RECORD_FLAG} needs a directory and a test binary");
        return 2;
    }
    let written = Record::of_this_process(argv.to_vec())
        .and_then(|r| write_record(Path::new(dir), &r.encode()));
    match written {
        Ok(()) => 0,
        Err(e) => {
            eprintln!(
                "aterm-verify: {RECORD_FLAG}: cannot record {} in {}: {e}",
                Path::new(&argv[0]).display(),
                Path::new(dir).display()
            );
            1
        }
    }
}

fn write_record(dir: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    for seq in 0..1_000_000u32 {
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        match open.open(dir.join(format!("{seq:06}.rec"))) {
            Ok(mut f) => return f.write_all(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("a million records in one directory"))
}

/// Every record in `dir`, in the order cargo made them.
///
/// # Errors
/// When the directory or a record cannot be read, or a record is not one.
pub fn read_records(dir: &Path) -> Result<Vec<Record>, String> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rec"))
        .collect();
    names.sort();
    names
        .iter()
        .map(|p| {
            let bytes =
                std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            Record::decode(&bytes).ok_or_else(|| format!("{} is not a test record", p.display()))
        })
        .collect()
}

/// The `--config` value that makes cargo run `gate --record-test-binary dir`
/// in place of every test binary: TOML, an array, so no path is split on a
/// space. `None` when a path is not UTF-8 and cannot be written into it.
#[must_use]
pub fn runner_config(gate: &Path, dir: &Path) -> Option<String> {
    let q = |s: &str| {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    };
    Some(format!(
        "target.'cfg(all())'.runner=[{},{},{}]",
        q(gate.to_str()?),
        q(RECORD_FLAG),
        q(dir.to_str()?)
    ))
}

/// The serial `--tests` child with the recording runner added before its
/// libtest separator — the same selection, flags and environment.
#[must_use]
pub fn recording_cmd(serial: &Cmd, config: &str) -> Cmd {
    let mut c = serial.clone();
    let at = c
        .args
        .iter()
        .position(|a| a == "--")
        .unwrap_or(c.args.len());
    c.args
        .splice(at..at, [OsString::from("--config"), OsString::from(config)]);
    c
}

// ---------------------------------------------------------------------------
// What the compile reported.
// ---------------------------------------------------------------------------

/// A test executable the compile built, with cargo's target flag for it
/// (`--lib`, `--bin <name>`, `--test <name>`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestExe {
    pub exe: PathBuf,
    pub flag: String,
}

/// cargo's re-run flag for a target of these kinds and this name.
fn target_flag(kinds: &[&str], name: &str) -> Option<String> {
    const LIB: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];
    if kinds.iter().any(|k| LIB.contains(k)) {
        return Some("--lib".to_string());
    }
    let which = match *kinds.first()? {
        "bin" => "bin",
        "test" => "test",
        "bench" => "bench",
        "example" => "example",
        _ => return None,
    };
    Some(format!("--{which} {name}"))
}

/// Take cargo's JSON messages out of a `--message-format=json-render-diagnostics`
/// log, in place — what is left is what the ladder shows, as it did before the
/// JSON was asked for — and return every test executable they report
/// (`compiler-artifact` with `profile.test` and an `executable`).
///
/// # Errors
/// A message that does not parse: the list would be short, so it is no list.
pub fn take_test_executables(log: &mut String) -> Result<Vec<TestExe>, String> {
    let mut exes = Vec::new();
    let mut bad = None;
    let mut kept = String::with_capacity(log.len());
    for line in log.split_inclusive('\n') {
        let t = line.trim_end_matches(['\n', '\r']);
        if !t.starts_with("{\"reason\":") {
            kept.push_str(line);
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(t) else {
            bad.get_or_insert_with(|| t.chars().take(120).collect::<String>());
            continue;
        };
        if v.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let test = v
            .get("profile")
            .and_then(|p| p.get("test"))
            .and_then(aterm_json::Value::as_bool)
            == Some(true);
        let Some(exe) = v.get("executable").and_then(aterm_json::Value::as_str) else {
            continue;
        };
        if !test {
            continue;
        }
        let target = v.get("target");
        let kinds: Vec<&str> = target
            .and_then(|t| t.get("kind"))
            .and_then(aterm_json::Value::as_array)
            .map(|a| a.iter().filter_map(aterm_json::Value::as_str).collect())
            .unwrap_or_default();
        let name = target
            .and_then(|t| t.get("name"))
            .and_then(aterm_json::Value::as_str)
            .unwrap_or_default();
        match target_flag(&kinds, name) {
            Some(flag) => exes.push(TestExe {
                exe: PathBuf::from(exe),
                flag,
            }),
            None => {
                bad.get_or_insert_with(|| format!("a test executable of kind {kinds:?}: {exe}"));
            }
        }
    }
    *log = kept;
    match bad {
        Some(what) => Err(format!("cargo reported what this gate cannot read: {what}")),
        None => Ok(exes),
    }
}

// ---------------------------------------------------------------------------
// The plan: records, cargo's headers and the compile's list, in agreement.
// ---------------------------------------------------------------------------

/// One test binary the run will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    /// cargo's `     Running <target> (<path>)` line for it, verbatim.
    pub header: String,
    /// cargo's re-run spec: `-p <package> <flag>`.
    pub spec: String,
    pub record: Record,
}

/// The `Running` headers of a cargo test log, in order, with each one's path.
fn announced(log: &str) -> Vec<(String, String)> {
    log.lines()
        .filter_map(|l| {
            let desc = crate::libtest::header(l)?;
            if !l.trim_start().starts_with("Running ") {
                return None;
            }
            let path = crate::libtest::binary_path(&desc)?.to_string();
            Some((l.trim_end().to_string(), path))
        })
        .collect()
}

/// Pair each record with the header cargo printed for it and the compile's
/// flag for its executable.
///
/// # Errors
/// Whatever does not add up, in a few words — the caller then runs the serial
/// child instead.
pub fn plan(records: Vec<Record>, log: &str, exes: &[TestExe]) -> Result<Vec<Binary>, String> {
    let headers = announced(log);
    if headers.len() != records.len() {
        return Err(format!(
            "cargo announced {} test binaries and the recorder saw {}",
            headers.len(),
            records.len()
        ));
    }
    records
        .into_iter()
        .zip(headers)
        .map(|(record, (header, path))| {
            let exe = record.exe().to_path_buf();
            if !exe.ends_with(&path) {
                return Err(format!(
                    "cargo announced {path} where the recorder saw {}",
                    exe.display()
                ));
            }
            let flag = exes
                .iter()
                .find(|e| e.exe == exe)
                .map(|e| e.flag.clone())
                .ok_or_else(|| format!("the compile did not report {}", exe.display()))?;
            let package = record
                .var("CARGO_PKG_NAME")
                .and_then(OsStr::to_str)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| format!("cargo gave {} no CARGO_PKG_NAME", exe.display()))?
                .to_string();
            let spec = format!("-p {package} {flag}");
            Ok(Binary {
                header,
                spec,
                record,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Running them.
// ---------------------------------------------------------------------------

/// Each shared binary's last wall time, in the run's state dir
/// ([`crate::Ctx::state_dir`]), so the next run starts the longest first.
pub const COSTS_FILE: &str = "test-costs.tsv";

/// The order the shared binaries START in (their logs still print in
/// declared order): the ones `costs` does not name first, then the rest
/// longest first. In cargo's order the longest binaries can start last and
/// run on alone at the end; longest first, they overlap the short ones
/// instead. Stable, so no history is declared order.
#[must_use]
pub fn start_order(shared: &[usize], bins: &[Binary], costs: &BTreeMap<String, f64>) -> Vec<usize> {
    let cost = |i: usize| costs.get(bins[i].spec.as_str());
    let mut order = shared.to_vec();
    order.sort_by(|&a, &b| match (cost(a), cost(b)) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => y.total_cmp(x),
    });
    order
}

/// The recorded wall seconds by spec (`-p <package> <flag>`) in `path`; a
/// missing or unreadable file is no history.
#[must_use]
pub fn read_costs(path: &Path) -> BTreeMap<String, f64> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|l| {
                    let (spec, secs) = l.rsplit_once('\t')?;
                    Some((spec.to_string(), secs.parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Merge this run's `measured` times into the history at `path` (a binary
/// this run did not time keeps its row), written whole and renamed into
/// place. A history that cannot be written only costs the next run its order.
pub fn write_costs(path: &Path, measured: &[(String, f64)]) {
    let mut costs = read_costs(path);
    for (spec, secs) in measured {
        costs.insert(spec.clone(), *secs);
    }
    let text: String = costs
        .iter()
        .map(|(spec, secs)| format!("{spec}\t{secs:.1}\n"))
        .collect();
    let tmp = path.with_extension("tsv.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Run every binary through [`pool_within`] under `limits`, in [`start_order`] over
/// `costs`, `load` reading the machine's busy cores. EVERY binary runs
/// with the whole `threads`, as cargo's own run hands each one.
/// `run_one(binary, its threads)` runs one; the answers come back in declared
/// order whatever order they ran in, beside the most shared binaries that ran
/// at once.
pub fn run_all<F, L>(
    bins: &[Binary],
    limits: Limits,
    threads: u32,
    costs: &BTreeMap<String, f64>,
    load: L,
    run_one: F,
) -> (Vec<Run>, usize)
where
    F: Fn(&Binary, u32) -> Run + Sync,
    L: Fn() -> Option<f64> + Sync,
{
    let all: Vec<usize> = (0..bins.len()).collect();
    let order = start_order(&all, bins, costs);
    let (ran, peak) = pool_within(bins.len(), &order, limits, load, |i| {
        run_one(&bins[i], threads)
    });
    let runs = ran
        .into_iter()
        .map(|pooled| {
            pooled.unwrap_or_else(|| Run {
                ok: false,
                output: "aterm-verify: this test binary was never run (a gate defect)\n".into(),
                code: None,
                spawn_error: None,
            })
        })
        .collect();
    (runs, peak)
}

/// How long one reading of the load serves every slot that asks, and how long
/// a slot the load holds back waits before it asks again.
pub const LOAD_SAMPLE: Duration = Duration::from_secs(1);

/// How many items a pool runs at once ([`pool_within`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Never more than this many at once (at least one).
    pub jobs: usize,
    /// Always up to this many while items are left, whatever the load (never
    /// above `jobs`).
    pub floor: usize,
    /// Past `floor`, one more starts only while the machine reads under this
    /// many busy cores ([`crate::Ctx::test_load`]).
    pub cores: u32,
    /// Past `floor`, how long after one start the next waits, and reads the
    /// machine afresh ([`ADMIT_SETTLE`]).
    pub settle: Duration,
}

/// How long a binary started past the floor gets to show its CPU before the
/// pool reads the machine for the next one. One reading serving every slot
/// that asked within [`LOAD_SAMPLE`] would let a second that read idle start
/// every free slot at once and overload the machine. A binary takes about a
/// second to load and fan out its threads.
pub const ADMIT_SETTLE: Duration = Duration::from_secs(2);

impl Limits {
    /// Exactly `jobs` at a time (at least one), the load never read.
    #[cfg(test)]
    #[must_use]
    pub fn fixed(jobs: usize) -> Self {
        let jobs = jobs.max(1);
        Self {
            jobs,
            floor: jobs,
            cores: 0,
            settle: Duration::ZERO,
        }
    }

    /// THE TEST RUN'S: up to `jobs` at once (at least one), always
    /// [`FLOOR_JOBS`] of them (within `jobs`), and more only while the machine
    /// reads under its `cores` busy (the cores busy over the last second,
    /// [`crate::exec::busy_cores`]; the one-minute load where that cannot be
    /// read). The reading is the whole
    /// machine's, the pool's own binaries included — what a test that waits on
    /// something feels — so the pool fills an idle machine and stops adding to a
    /// busy one; it never stops a binary it started.
    #[must_use]
    pub fn by_load(jobs: usize, cores: u32) -> Self {
        let jobs = jobs.max(1);
        Self {
            jobs,
            floor: FLOOR_JOBS.min(jobs),
            cores,
            settle: ADMIT_SETTLE,
        }
    }

    /// These limits with `settle` between starts past the floor.
    #[must_use]
    pub fn paced(self, settle: Duration) -> Self {
        Self { settle, ..self }
    }

    /// THE ADMISSION LAW, pure so it is a test: with `running` items running,
    /// may one more start? Under the floor, yes, and the load is not read;
    /// at the cap, no; between, only while `load()` reads under the cores. A
    /// load that cannot be read is not an idle machine: past the floor it is
    /// a no.
    #[must_use]
    pub fn may_start(&self, running: usize, load: impl FnOnce() -> Option<f64>) -> bool {
        running < self.jobs
            && (running < self.floor || load().is_some_and(|l| l < f64::from(self.cores)))
    }
}

/// Run `run(i)` for every index in `order` under `limits`: each item starts, in `order`, when
/// [`Limits::may_start`] says one more may — `load` is read at most once per
/// [`LOAD_SAMPLE`], and only when its answer decides that — and never stops
/// once started. The answers come back by index, of `n`, with `None` for an
/// index `order` does not hold, beside the most items that ran at once.
/// Nothing is retried; a `run` that panics is this call's panic once the
/// others are done, as a scoped thread's is.
pub fn pool_within<T, F, L>(
    n: usize,
    order: &[usize],
    limits: Limits,
    load: L,
    run: F,
) -> (Vec<Option<T>>, usize)
where
    T: Send,
    F: Fn(usize) -> T + Sync,
    L: Fn() -> Option<f64> + Sync,
{
    /// What the slots share, under one lock so that deciding to start and
    /// taking the next item are one step: the next item, how many run, the
    /// most that ever did, and the last reading of the load with its time.
    struct State {
        next: usize,
        running: usize,
        peak: usize,
        sampled: Option<(Instant, Option<f64>)>,
        started_past_floor: Option<Instant>,
    }
    /// One running item's claim on `running`, given back when it ends — by a
    /// panic too, so a slot that died never holds the floor.
    struct Claim<'a>(&'a Mutex<State>);
    impl Drop for Claim<'_> {
        fn drop(&mut self) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .running -= 1;
        }
    }
    /// What a slot does next.
    enum Step<'a> {
        Run(Claim<'a>, usize),
        Wait(Duration),
        Done,
    }
    let answers: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let state = Mutex::new(State {
        next: 0,
        running: 0,
        peak: 0,
        sampled: None,
        started_past_floor: None,
    });
    std::thread::scope(|scope| {
        for _ in 0..limits.jobs.clamp(1, order.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let step = {
                        let mut st = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let running = st.running;
                        match order.get(st.next).copied() {
                            None => Step::Done,
                            Some(i) => {
                                let past_floor = running >= limits.floor;
                                let settling = past_floor
                                    && st
                                        .started_past_floor
                                        .is_some_and(|at| at.elapsed() < limits.settle);
                                let may = !settling
                                    && limits.may_start(running, || match st.sampled {
                                        Some((at, v)) if at.elapsed() < LOAD_SAMPLE => v,
                                        _ => {
                                            let v = load();
                                            st.sampled = Some((Instant::now(), v));
                                            v
                                        }
                                    });
                                if may {
                                    if past_floor {
                                        // The next start past the floor waits out
                                        // the settle; a settle past LOAD_SAMPLE
                                        // also outlives this reading, so that start
                                        // reads the machine afresh.
                                        st.started_past_floor = Some(Instant::now());
                                    }
                                    st.next += 1;
                                    st.running += 1;
                                    st.peak = st.peak.max(st.running);
                                    Step::Run(Claim(&state), i)
                                } else if settling {
                                    Step::Wait(
                                        st.started_past_floor
                                            .map_or(LOAD_SAMPLE, |at| {
                                                limits.settle.saturating_sub(at.elapsed())
                                            })
                                            .min(LOAD_SAMPLE),
                                    )
                                } else {
                                    Step::Wait(LOAD_SAMPLE)
                                }
                            }
                        }
                    };
                    match step {
                        Step::Done => break,
                        Step::Wait(nap) => std::thread::sleep(nap),
                        Step::Run(_claim, i) => {
                            let out = run(i);
                            *answers[i].lock().expect("answer") = Some(out);
                        }
                    }
                }
            });
        }
    });
    let peak = state
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .peak;
    let answers = answers
        .into_iter()
        .map(|s| s.into_inner().ok().flatten())
        .collect();
    (answers, peak)
}

/// The command that runs `b` as cargo would have, with `threads` test threads
/// and its `Running` header at the head of its log.
#[must_use]
pub fn binary_cmd(b: &Binary, threads: u32) -> Cmd {
    let mut env: Vec<(OsString, OsString)> = b
        .record
        .env
        .iter()
        .filter(|(k, _)| k != "RUST_TEST_THREADS")
        .cloned()
        .collect();
    env.push(("RUST_TEST_THREADS".into(), threads.to_string().into()));
    Cmd::replayed(&b.record.argv, &b.record.cwd, env).preamble(format!("{}\n", b.header))
}

/// The one log cargo would have written for these runs, in declared order —
/// each binary's own log (header first), and after a failed one cargo's
/// `error: test failed, to rerun pass` line (with its `Caused by` when the
/// binary did not exit with libtest's 101) — then cargo's closing list of the
/// targets that failed. `ok` only when every binary passed.
#[must_use]
pub fn combined(bins: &[Binary], runs: &[Run]) -> Run {
    let mut output = String::new();
    let mut failed = Vec::new();
    for (b, r) in bins.iter().zip(runs) {
        output.push_str(&r.output);
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if r.ok {
            continue;
        }
        output.push_str(&format!("error: test failed, to rerun pass `{}`\n", b.spec));
        if r.code != Some(101) {
            let argv: Vec<String> = b
                .record
                .argv
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let why = match (&r.spawn_error, r.code) {
                (Some(e), _) => format!("could not execute: {e}"),
                (None, Some(c)) => format!("exit status: {c}"),
                (None, None) => "killed by a signal".to_string(),
            };
            output.push_str(&format!(
                "\nCaused by:\n  process didn't exit successfully: `{}` ({why})\n",
                argv.join(" ")
            ));
        }
        failed.push(b.spec.as_str());
    }
    if !failed.is_empty() {
        output.push_str(&format!(
            "error: {} target{} failed:\n",
            failed.len(),
            if failed.len() == 1 { "" } else { "s" }
        ));
        for spec in &failed {
            output.push_str(&format!("    `{spec}`\n"));
        }
    }
    Run {
        ok: failed.is_empty(),
        output,
        code: Some(if failed.is_empty() { 0 } else { 101 }),
        spawn_error: None,
    }
}

/// The ladder's one line about how the binaries ran: every one with `threads`
/// test threads; `timed` of the shared ones had a time in the history
/// ([`COSTS_FILE`]), and at most `peak` of them ran at once under `limits`.
#[must_use]
pub fn summary(bins: &[Binary], limits: Limits, threads: u32, timed: usize, peak: usize) -> String {
    let n = bins.len();
    let mut s = format!("  test binaries: {n} — RUST_TEST_THREADS={threads} each, ");
    let timed = timed.min(n);
    // [`longest_first`] is stable: with no times it keeps cargo's order.
    s.push_str(&match n - timed {
        _ if timed == 0 => "in cargo's order (no times recorded yet), ".to_string(),
        0 => "longest first by the last run's times, ".to_string(),
        untimed => {
            format!("the {untimed} never timed first, then longest first by the last run's times, ")
        }
    });
    if limits.jobs > limits.floor {
        s.push_str(&format!(
            "up to {} at once, past {} only while fewer than {} cores read busy: \
             {peak} at the most",
            limits.jobs, limits.floor, limits.cores
        ));
    } else {
        s.push_str(&format!("{} at a time", limits.jobs.min(n.max(1))));
    }
    s
}

/// A binary the test run's ceiling left no time to start: a FAIL in cargo's
/// shape (its header, then why), never a pass and never a skip; it says
/// [`crate::findings::CEILING_KILL`].
#[must_use]
pub fn not_started(b: &Binary, limit: std::time::Duration) -> Run {
    Run {
        ok: false,
        output: format!(
            "{}\n{} — not started: the test run's {:.1}s wall-clock ceiling was spent before \
             this binary's turn\n\x20 test binary: {}\n\x20 Counted as a FAIL: it never ran.\n\
             \x20 Raise the ceiling with --timeout <seconds>, or remove it with \
             --timeout off.\n",
            b.header,
            crate::findings::CEILING_KILL,
            limit.as_secs_f64(),
            b.spec
        ),
        code: None,
        spawn_error: None,
    }
}

/// A line of a failed recording pass worth quoting: its first diagnostic, or
/// its last line.
fn first_error(log: &str) -> String {
    let line = log
        .lines()
        .find(|l| l.trim_start().starts_with("error"))
        .or_else(|| log.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("no output");
    line.trim().chars().take(200).collect()
}

/// Decide the test run's row exactly as the serial child's was decided, then
/// print the slowest tests its `--report-time` verdicts timed
/// ([`crate::libtest::slowest`]; nothing when none were timed).
///
/// A binary whose tests of a macOS service (`trustd`, the disk-image
/// service) returned early because it did not answer passed without checking
/// them: the row stays what ran decided, and ONE named skip per service says
/// what did not ([`crate::libtest::host_skips`]).
fn decide(r: &mut Report, run: &Run, label: &str, env: &[(OsString, OsString)]) {
    r.decide_test_child(run, label, env);
    for skip in crate::libtest::host_skips(&run.output) {
        r.skip(skip);
    }
    if let Some(table) = crate::libtest::slowest(&run.output, crate::stages::SLOWEST_TESTS_LISTED) {
        r.raw(table);
    }
}

/// THE TEST RUN (the test stage's second half, after its compile): record,
/// plan, run, and decide one row labeled `label`, exactly as the serial
/// `serial` child's row was decided — see the module doc for every path.
/// `exes` is what the compile reported ([`take_test_executables`]).
pub fn run_tests(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    serial: &Cmd,
    exes: Result<Vec<TestExe>, String>,
) {
    let env = crate::findings::rerun_env(serial, ctx.test_threads);
    let serially = |r: &mut Report, why: &str| {
        r.raw(format!(
            "  test binaries: run by cargo, one at a time — {why}"
        ));
        let out = exec::run(serial, ctx.exec_env());
        r.raw(out.output.as_str());
        decide(r, &out, label, &env);
    };
    let dir = ctx.scratch.join(format!(
        "test-records.{}.{}",
        std::process::id(),
        RECORDING.fetch_add(1, Ordering::SeqCst)
    ));
    let gate = ctx.test_recorder();
    let config = match (&gate, std::fs::create_dir_all(&dir)) {
        (Some(gate), Ok(())) => runner_config(gate, &dir),
        _ => None,
    };
    let Some(config) = config else {
        std::fs::remove_dir_all(&dir).ok();
        serially(r, "verify could not name itself as the recording runner");
        return;
    };
    let pass = exec::run(&recording_cmd(serial, &config), ctx.exec_env());
    let records = read_records(&dir);
    std::fs::remove_dir_all(&dir).ok();

    // RECORDS THAT CANNOT BE READ ARE NEVER "NO RECORDS": the pass behind them
    // may have executed nothing — every binary replaced by a recorder that
    // exited 0 — and judged as the serial run it was not, it would be an `ok`
    // row with no test run. The serial child decides instead.
    if let Err(why) = &records {
        serially(r, &format!("the recording could not be read: {why}"));
        return;
    }
    // The recorder ran if it recorded something, or if cargo reports it
    // failing (`process didn't exit successfully: \`<gate> --record-test-binary
    // …\``): a recorder that could not write is the run's failure, never the
    // tree's, and the serial child below decides instead.
    let recorded = records.as_ref().is_ok_and(|v| !v.is_empty());
    let recorder_failed = pass
        .output
        .lines()
        .any(|l| l.contains("didn't exit successfully") && l.contains(RECORD_FLAG));
    // A pass that announced binaries and printed no `test result:` ran none
    // of them: it is never judged as the serial run.
    let ran_nothing = !announced(&pass.output).is_empty()
        && !pass.output.lines().any(|l| l.starts_with("test result: "));
    if !recorded && !recorder_failed && ran_nothing {
        serially(
            r,
            "cargo announced test binaries, and neither they nor the recorder left a result",
        );
        return;
    }
    if !recorded && !recorder_failed {
        // The recorder never ran: no test binary at all, or a runner the
        // caller's cargo config names outranked this one and cargo ran every
        // binary itself — this pass WAS the serial run.
        if !announced(&pass.output).is_empty() {
            r.raw(
                "  test binaries: run by cargo, one at a time — the recording runner was not \
                 used (a runner in the caller's cargo config outranks it)",
            );
        }
        r.raw(pass.output.as_str());
        decide(r, &pass, label, &env);
        return;
    }
    let bins = if pass.ok {
        records
            .and_then(|recs| exes.and_then(|exes| plan(recs, &pass.output, &exes)))
            .map_err(|why| format!("the recording did not add up: {why}"))
    } else {
        Err(format!(
            "the recording pass failed: {}",
            first_error(&pass.output)
        ))
    };
    let bins = match bins {
        Ok(bins) => bins,
        Err(why) => {
            serially(r, &why);
            return;
        }
    };
    // Three quarters of the cores: the binaries running burst past their
    // average, so a pool that admits until every core reads busy has none left
    // for the burst.
    let limits = Limits::by_load(
        ctx.test_jobs()
            .min(memory_jobs(ctx.host_memory, &crate::plan::plan(ctx))),
        (ctx.cores() * 3 / 4).max(1),
    );
    let threads = ctx.test_threads();
    // ONE CEILING FOR THE WHOLE RUN, as the serial child had: a binary is
    // started with what is left of it, and one whose turn comes after it is
    // spent is not started. A full ceiling per binary would leave the stage no
    // bound at all — two hangs, two ceilings.
    let ceiling = ctx.exec_env().child_ceiling;
    let started = std::time::Instant::now();
    let costs_path = Some(ctx.state_dir().join(COSTS_FILE));
    let costs = costs_path.as_deref().map(read_costs).unwrap_or_default();
    let timed = bins.iter().filter(|b| costs.contains_key(&b.spec)).count();
    let measured: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
    let cpu: Mutex<Vec<Busy>> = Mutex::new(Vec::new());
    let (runs, peak) = run_all(
        &bins,
        limits,
        threads,
        &costs,
        || ctx.test_load(),
        |b, t| {
            let mut env = ctx.exec_env();
            if let Some(limit) = ceiling {
                let left = limit.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return not_started(b, limit);
                }
                env.child_ceiling = Some(left);
            }
            let began = std::time::Instant::now();
            let (run, spent) = exec::run_measured(&binary_cmd(b, t), env);
            let wall = began.elapsed().as_secs_f64();
            measured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((b.spec.clone(), wall));
            if let Some(spent) = spent {
                cpu.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(Busy {
                        spec: b.spec.clone(),
                        wall,
                        cpu: spent.as_secs_f64(),
                    });
            }
            run
        },
    );
    if let Some(path) = &costs_path {
        write_costs(
            path,
            &measured
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
    // cargo's own lines from the pass (its warnings, `Finished`), without the
    // headers the binaries' logs carry.
    let preface: String = pass
        .output
        .lines()
        .filter(|l| !l.trim_start().starts_with("Running "))
        .map(|l| format!("{l}\n"))
        .collect();
    r.raw(preface);
    r.raw(summary(&bins, limits, threads, timed, peak));
    let all = combined(&bins, &runs);
    r.raw(all.output.as_str());
    decide(r, &all, label, &env);
    if let Some(table) = busiest(
        cpu.into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        bins.len(),
        BUSIEST_LISTED,
    ) {
        r.raw(table);
    }
}

/// One test binary's cost in the test run: its wall seconds and the CPU
/// seconds it and the processes it reaped spent ([`exec::run_measured`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Busy {
    pub spec: String,
    pub wall: f64,
    pub cpu: f64,
}

/// How many binaries [`busiest`] lists.
pub const BUSIEST_LISTED: usize = 12;

/// The binaries that spent the most CPU, most first, beside their wall time,
/// under the run's total: on a saturated machine the test run's length follows
/// the CPU every binary spends, which a binary's own wall time does not show.
/// `planned` is how many binaries the run had: one with no
/// CPU figure (a FAIL above names why) is left out, and the header says how
/// many the table and its total leave out. `None` when no binary was measured
/// (off Unix).
#[must_use]
pub fn busiest(mut runs: Vec<Busy>, planned: usize, n: usize) -> Option<String> {
    if runs.is_empty() {
        return None;
    }
    let total: f64 = runs.iter().map(|b| b.cpu).sum();
    runs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then_with(|| a.spec.cmp(&b.spec)));
    let measured = runs.len();
    let gap = if planned > measured {
        format!("; {} not measured", planned - measured)
    } else {
        String::new()
    };
    let mut out = format!(
        "  busiest test binaries by CPU ({} of {measured}; {total:.0} CPU-s in all{gap}):\n",
        n.min(measured)
    );
    for b in runs.iter().take(n) {
        out.push_str(&format!(
            "    {:8.1} CPU-s {:7.1}s wall  {}\n",
            b.cpu, b.wall, b.spec
        ));
    }
    Some(out)
}

static RECORDING: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    /// A test run whose binaries a silent macOS service skipped stays green
    /// for what ran, and ONE named skip per service says what did not and
    /// what clears it; without the line, nothing changes.
    #[test]
    fn a_silent_service_is_one_named_skip_beside_a_green_row() {
        let judge = |output: String| {
            let run = Run {
                ok: true,
                output,
                code: Some(0),
                spawn_error: None,
            };
            let mut r = Report::new("tests");
            decide(&mut r, &run, "targo test --workspace", &[]);
            crate::ladder::tally(&[r])
        };
        let log = |skipped: bool| {
            let line = if skipped {
                format!("{}\n", crate::libtest::skipped::SKIPPED)
            } else {
                String::new()
            };
            format!(
                "     Running unittests src/lib.rs (target/debug/deps/aterm_uds-1f2e)\n\
                 {line}running 1 test\ntest t::signs ... ok\n\
                 test result: ok. 1 passed; 0 failed\n"
            )
        };
        let t = judge(log(true));
        assert_eq!(
            t.skips,
            [
                "tests: trustd did not answer — 1 test binary skipped its code-signing and TLS \
                 checks (aterm_uds unittests src/lib.rs); rebooting the Mac clears a wedged trustd"
            ]
        );
        assert!(!t.failed());
        let t = judge(log(true).replace(
            crate::libtest::skipped::SKIPPED,
            crate::libtest::skipped::DISK_IMAGE_SKIPPED,
        ));
        assert_eq!(
            t.skips,
            [
                "tests: hdiutil did not answer — 1 test binary skipped its disk-image checks \
                 (aterm_uds unittests src/lib.rs); rebooting the Mac clears a wedged disk-image \
                 service"
            ]
        );
        assert!(judge(log(false)).skips.is_empty());
    }

    /// The busiest table lists the binaries that spent the most CPU, most first
    /// (ties by name), at most `n`, under the run's total; nothing measured is
    /// no table.
    #[test]
    fn the_busiest_table_ranks_by_cpu_and_names_the_total() {
        let b = |spec: &str, wall: f64, cpu: f64| Busy {
            spec: spec.to_string(),
            wall,
            cpu,
        };
        assert_eq!(busiest(Vec::new(), 3, 3), None);
        let runs = vec![
            b("-p a --lib", 100.0, 20.0),
            b("-p b --lib", 10.0, 90.0),
            b("-p c --lib", 5.0, 20.0),
        ];
        let table = busiest(runs.clone(), 3, 2).expect("a table");
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[0].contains("(2 of 3; 130 CPU-s in all):"), "{table}");
        assert!(
            lines[1].ends_with("-p b --lib") && lines[1].contains("90.0 CPU-s"),
            "{table}"
        );
        assert!(lines[2].ends_with("-p a --lib"), "ties by name: {table}");
        assert_eq!(lines.len(), 3, "{table}");
        // A binary with no CPU figure is left out: the header names how many
        // the table and its total leave out.
        let short = busiest(runs, 5, 2).expect("a table");
        assert!(
            short
                .lines()
                .next()
                .unwrap()
                .contains("130 CPU-s in all; 2 not measured)"),
            "{short}"
        );
    }

    fn rec(exe: &str, pkg: &str) -> Record {
        Record {
            cwd: PathBuf::from(format!("/r/crates/{pkg}")),
            argv: vec![
                exe.into(),
                "-Zunstable-options".into(),
                "--report-time".into(),
            ],
            env: vec![
                ("CARGO_PKG_NAME".into(), pkg.into()),
                ("RUST_TEST_THREADS".into(), "14".into()),
                ("CARGO".into(), "/store/bin/targo".into()),
            ],
        }
    }

    fn artifact(exe: &str, kind: &str, name: &str, test: bool) -> String {
        format!(
            "{{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///r/crates/x#0.1.0\",\
             \"target\":{{\"kind\":[\"{kind}\"],\"crate_types\":[\"bin\"],\"name\":\"{name}\",\
             \"src_path\":\"/r/crates/x/src/lib.rs\",\"test\":true}},\
             \"profile\":{{\"opt_level\":\"0\",\"debuginfo\":2,\"test\":{test}}},\
             \"features\":[],\"filenames\":[\"{exe}\"],\"executable\":\"{exe}\",\"fresh\":true}}\n"
        )
    }

    /// A RECORD IS THE PROCESS, BYTE FOR BYTE: every argument and variable
    /// survives, spaces, `=` and newlines included, and anything that is not a
    /// whole record is refused rather than read short.
    #[test]
    fn a_record_round_trips_and_a_torn_one_is_refused() {
        let r = Record {
            cwd: PathBuf::from("/a dir/with space"),
            argv: vec!["/t/x-1".into(), "--skip".into(), "a b\nc".into()],
            env: vec![
                ("K".into(), "v=w".into()),
                ("EMPTY".into(), "".into()),
                ("NL".into(), "one\ntwo".into()),
            ],
        };
        let bytes = r.encode();
        assert_eq!(Record::decode(&bytes), Some(r.clone()));
        assert_eq!(r.var("NL"), Some(OsStr::new("one\ntwo")));
        assert_eq!(Record::decode(&bytes[..bytes.len() - 1]), None, "cut short");
        assert_eq!(
            Record::decode(&bytes[..bytes.len() - 3]),
            None,
            "cut mid-token"
        );
        assert_eq!(Record::decode(b"something else\0"), None);
    }

    /// THE RECORDER, THROUGH ITS OWN ENTRY POINT: each call takes the next
    /// number, the file is the owner's alone, and what comes back is this
    /// process — its directory and its environment — with the argv it was given.
    #[test]
    fn the_recorder_writes_the_next_owner_only_record() {
        let dir = crate::mktemp_dir("atv-rec").expect("mktemp");
        let d = dir.clone().into_os_string();
        assert_eq!(
            record_main(&[d.clone(), "/t/a".into(), "--skip".into(), "m::".into()]),
            0
        );
        assert_eq!(record_main(&[d.clone(), "/t/b".into()]), 0);
        assert_eq!(record_main(&[d]), 2, "no test binary is a usage error");
        let recs = read_records(&dir).expect("records");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].argv, ["/t/a", "--skip", "m::"]);
        assert_eq!(recs[1].argv, ["/t/b"]);
        assert_eq!(recs[0].cwd, std::env::current_dir().expect("cwd"));
        assert_eq!(recs[0].var("PATH"), std::env::var_os("PATH").as_deref());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("000000.rec"))
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the environment is the owner's alone: {mode:o}"
            );
        }
        let unwritable = dir.join("absent/deeper").into_os_string();
        assert_eq!(record_main(&[unwritable, "/t/c".into()]), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_runner_config_is_a_toml_array_so_no_path_splits() {
        assert_eq!(
            runner_config(Path::new("/a b/aterm-verify"), Path::new("/tmp/q\"d")).as_deref(),
            Some(
                "target.'cfg(all())'.runner=[\"/a b/aterm-verify\",\"--record-test-binary\",\
                 \"/tmp/q\\\"d\"]"
            )
        );
        let serial = Cmd::new("targo").args([
            "--unverified",
            "test",
            "--workspace",
            "--no-fail-fast",
            "--tests",
            "--",
            "-Zunstable-options",
            "--report-time",
        ]);
        assert_eq!(
            recording_cmd(&serial, "CFG").argv(),
            [
                "targo",
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--tests",
                "--config",
                "CFG",
                "--",
                "-Zunstable-options",
                "--report-time"
            ],
            "cargo's flag before the separator, libtest's arguments untouched after it"
        );
    }

    /// THE COMPILE'S LIST, AND THE LOG WITHOUT IT: only test-mode executables
    /// count, each with cargo's own flag, and every JSON line leaves the text
    /// the ladder shows — the diagnostics and progress lines stay, in order.
    #[test]
    fn the_compile_names_its_test_executables_and_keeps_its_diagnostics() {
        let mut log = format!(
            "   Compiling x v0.1.0\n{}{}{}{}warning: unused\n{}{{\"reason\":\"build-finished\",\
             \"success\":true}}\n    Finished `test` profile\n",
            artifact("/r/target/debug/deps/x-1", "lib", "x", true),
            artifact("/r/target/debug/x", "bin", "x", false),
            artifact("/r/target/debug/deps/x-2", "bin", "x", true),
            artifact("/r/target/debug/deps/it-3", "test", "it", true),
            artifact("/r/target/debug/deps/ex-4", "example", "ex", true),
        );
        let exes = take_test_executables(&mut log).expect("readable");
        assert_eq!(
            exes.iter()
                .map(|e| (e.exe.to_str().unwrap(), e.flag.as_str()))
                .collect::<Vec<_>>(),
            [
                ("/r/target/debug/deps/x-1", "--lib"),
                ("/r/target/debug/deps/x-2", "--bin x"),
                ("/r/target/debug/deps/it-3", "--test it"),
                ("/r/target/debug/deps/ex-4", "--example ex"),
            ]
        );
        assert_eq!(
            log,
            "   Compiling x v0.1.0\nwarning: unused\n    Finished `test` profile\n"
        );
        let mut torn = "{\"reason\":\"compiler-artifact\",\"target\":\n".to_string();
        assert!(
            take_test_executables(&mut torn).is_err(),
            "a torn message is no list"
        );
    }

    /// THE PLAN ADDS UP OR IT IS NOT A PLAN: every record meets the header cargo
    /// printed for it and the compile's flag for its executable, the re-run spec
    /// is cargo's.
    #[test]
    fn records_headers_and_the_compile_agree_or_the_plan_is_refused() {
        let log = "    Finished `test` profile\n\
                   \x20    Running unittests src/lib.rs (target/debug/deps/aterm_link-1)\n\
                   \x20    Running tests/bridge_e2e.rs (target/debug/deps/bridge_e2e-2)\n";
        let exes = [
            TestExe {
                exe: "/r/target/debug/deps/aterm_link-1".into(),
                flag: "--lib".into(),
            },
            TestExe {
                exe: "/r/target/debug/deps/bridge_e2e-2".into(),
                flag: "--test bridge_e2e".into(),
            },
        ];
        let recs = || {
            vec![
                rec("/r/target/debug/deps/aterm_link-1", "aterm-link"),
                rec("/r/target/debug/deps/bridge_e2e-2", "aterm-link"),
            ]
        };
        let bins = plan(recs(), log, &exes).expect("a plan");
        assert_eq!(
            bins.iter().map(|b| b.spec.as_str()).collect::<Vec<_>>(),
            ["-p aterm-link --lib", "-p aterm-link --test bridge_e2e"]
        );
        assert_eq!(
            bins[1].header,
            "     Running tests/bridge_e2e.rs (target/debug/deps/bridge_e2e-2)"
        );

        let mut one_short = recs();
        one_short.pop();
        assert!(
            plan(one_short, log, &exes)
                .unwrap_err()
                .contains("recorder saw 1")
        );
        let mut swapped = recs();
        swapped.swap(0, 1);
        assert!(
            plan(swapped, log, &exes)
                .unwrap_err()
                .contains("where the recorder saw")
        );
        assert!(
            plan(recs(), log, &exes[..1])
                .unwrap_err()
                .contains("did not report")
        );
        let mut nameless = recs();
        nameless[0].env.retain(|(k, _)| k != "CARGO_PKG_NAME");
        assert!(
            plan(nameless, log, &exes)
                .unwrap_err()
                .contains("CARGO_PKG_NAME")
        );
    }

    /// THE START ORDER: binaries the history does not name start first, then
    /// the named ones longest first; with no history it is declared order. The
    /// history round-trips through its file, a new run's times merged over it.
    #[test]
    fn shared_binaries_start_longest_first_by_the_recorded_times() {
        let bins: Vec<Binary> = (0..4)
            .map(|i| Binary {
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let all = [0, 1, 2, 3];
        assert_eq!(start_order(&all, &bins, &BTreeMap::new()), all);

        let dir = crate::mktemp_dir("atv-costs").expect("mktemp");
        let path = dir.join(COSTS_FILE);
        write_costs(
            &path,
            &[
                ("-p x --test t0".into(), 3.0),
                ("-p x --test t2".into(), 90.0),
            ],
        );
        write_costs(&path, &[("-p x --test t3".into(), 40.0)]);
        let costs = read_costs(&path);
        assert_eq!(costs.len(), 3, "{costs:?}");
        assert_eq!(start_order(&all, &bins, &costs), [1, 2, 3, 0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// THE SCHEDULE: never more than `jobs` at once, started longest first; EVERY
    /// binary with the whole thread count, as cargo's own run gives it; and the
    /// answers in declared order whatever order they finished in.
    #[test]
    fn binaries_share_the_pool_longest_first_and_answer_in_declared_order() {
        let bins: Vec<Binary> = (0..9)
            .map(|i| Binary {
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let order = Mutex::new(Vec::new());
        let run_one = |b: &Binary, threads: u32| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            assert_eq!(threads, 12, "every binary gets the whole count");
            order.lock().unwrap().push(b.spec.clone());
            std::thread::sleep(std::time::Duration::from_millis(if b.spec.ends_with('0') {
                60
            } else {
                5
            }));
            running.fetch_sub(1, Ordering::SeqCst);
            Run {
                ok: true,
                output: format!("{}\nran {}\n", b.header, b.spec),
                code: Some(0),
                spawn_error: None,
            }
        };
        // t8 took longest last time; t5 no run has timed.
        let costs: BTreeMap<String, f64> = (0..9)
            .filter(|&i| i != 5)
            .map(|i| (format!("-p x --test t{i}"), if i == 8 { 90.0 } else { 1.0 }))
            .collect();
        // One at a time the start order is the call order.
        let (_, one) = run_all(
            &bins,
            Limits::by_load(1, 64),
            12,
            &costs,
            || Some(0.0),
            run_one,
        );
        assert_eq!(one, 1);
        assert_eq!(
            std::mem::take(&mut *order.lock().unwrap()),
            [5, 8, 0, 1, 2, 3, 4, 6, 7].map(|i| format!("-p x --test t{i}")),
            "the untimed first, then longest first, then declared order"
        );
        assert_eq!(peak.swap(0, Ordering::SeqCst), 1);
        // Three at a time on an idle machine.
        let (runs, pooled) = run_all(
            &bins,
            Limits::by_load(3, 64),
            12,
            &BTreeMap::new(),
            || Some(0.0),
            run_one,
        );
        assert!(pooled <= 3 && peak.load(Ordering::SeqCst) <= 3);
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "the shared slots were shared"
        );
        let said: Vec<String> = runs
            .iter()
            .map(|r| r.output.lines().nth(1).unwrap().to_string())
            .collect();
        assert_eq!(
            said,
            (0..9)
                .map(|i| format!("ran -p x --test t{i}"))
                .collect::<Vec<_>>(),
            "declared order"
        );
    }

    /// THE ADMISSION LAW: under the floor, yes without reading the load; at the
    /// cap, no; between, only while the load is under the cores — a load AT
    /// the cores is not under them, and one that cannot be read is not an idle
    /// machine. `--test-jobs 1` is one at a time, and a fixed pool never asks.
    #[test]
    fn the_pool_keeps_the_floor_whatever_the_load_and_fills_only_an_idle_machine() {
        let never = || -> Option<f64> { panic!("the load was read") };
        let l = Limits::by_load(8, 18);
        assert_eq!((l.jobs, l.floor, l.cores), (8, FLOOR_JOBS, 18));
        assert!(l.may_start(0, never) && l.may_start(1, never));
        assert!(l.may_start(2, || Some(17.9)) && l.may_start(7, || Some(0.0)));
        assert!(!l.may_start(2, || Some(18.0)), "at the cores is not under");
        assert!(!l.may_start(2, || Some(90.0)));
        assert!(!l.may_start(2, || None), "an unreadable load is not idle");
        assert!(!l.may_start(8, || Some(0.0)), "never past the cap");
        let one = Limits::by_load(1, 18);
        assert_eq!(
            (one.jobs, one.floor),
            (1, 1),
            "the floor is never above the cap"
        );
        assert!(one.may_start(0, never) && !one.may_start(1, || Some(0.0)));
        assert_eq!(Limits::by_load(0, 18).jobs, 1, "never zero");
        let fixed = Limits::fixed(4);
        assert!((0..4).all(|r| fixed.may_start(r, never)));
        assert!(!fixed.may_start(4, never));
    }

    /// The most items at once `pool_within` reaches over `n` items, the load
    /// reading `load` (each reading counted in `reads`). Each item waits, a
    /// minute at the most (a hang detector), until `want` have run at once, so a loaded
    /// machine's late thread start cannot hide a pool's concurrency; a pool
    /// that never reaches `want` only makes this slower, never green.
    fn pool_peak(
        n: usize,
        limits: Limits,
        load: Option<f64>,
        want: usize,
        reads: &AtomicUsize,
    ) -> usize {
        let running = AtomicUsize::new(0);
        let seen = AtomicUsize::new(0);
        let order: Vec<usize> = (0..n).collect();
        let began = Instant::now();
        let (answers, peak) = pool_within(
            n,
            &order,
            limits,
            || {
                reads.fetch_add(1, Ordering::SeqCst);
                load
            },
            |i| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                seen.fetch_max(now, Ordering::SeqCst);
                let waited = Instant::now();
                while seen.load(Ordering::SeqCst) < want
                    && waited.elapsed() < Duration::from_secs(60)
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                std::thread::sleep(Duration::from_millis(10));
                running.fetch_sub(1, Ordering::SeqCst);
                i
            },
        );
        assert_eq!(
            answers,
            (0..n).map(Some).collect::<Vec<_>>(),
            "every item ran once"
        );
        assert!(
            seen.load(Ordering::SeqCst) <= peak,
            "the peak it reports is what ran"
        );
        let samples = began.elapsed().as_millis() / LOAD_SAMPLE.as_millis();
        let samples = usize::try_from(samples).unwrap_or(usize::MAX);
        assert!(
            reads.load(Ordering::SeqCst) <= 1 + samples,
            "at most one reading per LOAD_SAMPLE"
        );
        peak
    }

    /// THE POOL OVER THREADS: an idle machine fills to the cap; a busy one or
    /// one whose load cannot be read keeps the floor, and every item still
    /// runs; the floor and a fixed pool never read the load.
    #[test]
    fn a_busy_machine_keeps_the_floor_and_an_idle_one_fills_to_the_cap() {
        let peak = |n, limits, load, want| {
            let reads = AtomicUsize::new(0);
            let peak = pool_peak(n, limits, load, want, &reads);
            (peak, reads.into_inner())
        };
        let quick = |jobs| Limits::by_load(jobs, 18).paced(Duration::ZERO);
        assert_eq!(peak(12, quick(6), Some(1.0), 6).0, 6);
        assert_eq!(peak(5, quick(6), Some(40.0), 2).0, FLOOR_JOBS);
        assert_eq!(peak(5, quick(6), None, 2).0, FLOOR_JOBS);
        assert_eq!(peak(5, quick(2), Some(0.0), 2), (2, 0));
        assert_eq!(peak(9, Limits::fixed(3), Some(0.0), 3), (3, 0));
    }

    /// THE SETTLE: past the floor the pool starts one binary, then lets it show
    /// its CPU before the next, however idle the one reading looked; the floor
    /// itself starts at once.
    #[test]
    fn starts_past_the_floor_wait_out_the_settle() {
        let settle = Duration::from_millis(60);
        let began = Instant::now();
        let starts = Mutex::new(Vec::new());
        let (answers, peak) = pool_within(
            5,
            &[0, 1, 2, 3, 4],
            Limits::by_load(5, 64).paced(settle),
            || Some(0.0),
            |i| {
                starts.lock().unwrap().push(began.elapsed());
                // Hold the slot until every item has started.
                while starts.lock().unwrap().len() < 5 {
                    std::thread::sleep(Duration::from_millis(2));
                }
                i
            },
        );
        assert_eq!(answers, (0..5).map(Some).collect::<Vec<_>>());
        assert_eq!(peak, 5);
        let mut starts = starts.into_inner().unwrap();
        starts.sort();
        for w in starts[FLOOR_JOBS..].windows(2) {
            assert!(w[1] - w[0] >= settle, "spaced by the settle: {starts:?}");
        }
    }

    /// A RUN THAT PANICS is the pool's panic once the others are done — and
    /// its slot gives the floor back, so a busy machine's pool still runs
    /// every other item instead of waiting on a slot that died.
    #[test]
    fn a_panicking_run_is_a_panic_and_never_holds_the_floor() {
        let done = AtomicUsize::new(0);
        let order: Vec<usize> = (0..6).collect();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool_within(
                6,
                &order,
                Limits::by_load(4, 18),
                || Some(99.0),
                |i| {
                    assert!(i > 1, "run {i} broke");
                    done.fetch_add(1, Ordering::SeqCst);
                },
            )
        }));
        assert!(caught.is_err(), "the panic is the pool's");
        assert_eq!(done.load(Ordering::SeqCst), 4, "the other four ran");
    }

    /// A BINARY THE RUN'S CEILING LEFT NO TIME TO START is a FAIL in cargo's
    /// shape — its header, the run's TIMEOUT words, cargo's re-run
    /// line — one finding for the row, which says so.
    #[test]
    fn a_binary_the_ceiling_left_no_time_for_is_a_fail() {
        let b = Binary {
            header: "     Running tests/late.rs (target/debug/deps/late-1)".into(),
            spec: "-p x --test late".into(),
            record: rec("/r/late", "x"),
        };
        let run = not_started(&b, std::time::Duration::from_secs(10_800));
        assert!(!run.ok);
        assert!(run.output.starts_with(&b.header), "{}", run.output);
        assert!(
            run.output.contains(crate::findings::CEILING_KILL),
            "{}",
            run.output
        );
        let all = combined(std::slice::from_ref(&b), std::slice::from_ref(&run));
        assert!(!all.ok);
        let found = crate::findings::test_findings("targo test --tests", &all.output, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rerun, None, "a whole-row finding: {found:?}");
        assert!(
            found[0]
                .message
                .iter()
                .any(|l| l.contains(crate::findings::CEILING_KILL)),
            "{found:?}"
        );
    }

    /// THE COMBINED LOG IS CARGO'S, so every reader of cargo's reads it: the
    /// failed tests come back with the re-run spec cargo would have printed, a
    /// crash (no libtest 101) says how the process ended, and a binary whose
    /// every failure is the machine's still reads as the machine's.
    #[test]
    fn the_combined_log_reads_as_cargos_to_every_reader_of_cargos() {
        let bins: Vec<Binary> = ["a", "b", "c"]
            .iter()
            .map(|n| Binary {
                header: format!("     Running tests/{n}.rs (target/debug/deps/{n}-1)"),
                spec: format!("-p x --test {n}"),
                record: rec(&format!("/r/target/debug/deps/{n}-1"), "x"),
            })
            .collect();
        let pass = |b: &Binary| Run {
            ok: true,
            output: format!(
                "{}\n\nrunning 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 \
                 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
                b.header
            ),
            code: Some(0),
            spawn_error: None,
        };
        let fail = |b: &Binary| Run {
            ok: false,
            output: format!(
                "{}\n\nrunning 1 test\ntest t::broke ... FAILED\n\nfailures:\n\n\
                 ---- t::broke stdout ----\n\nthread 't::broke' panicked at src/x.rs:3:5:\n\
                 assertion failed: false\n\nfailures:\n    t::broke\n\ntest result: FAILED. 0 \
                 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
                b.header
            ),
            code: Some(101),
            spawn_error: None,
        };
        let runs = [pass(&bins[0]), fail(&bins[1]), pass(&bins[2])];
        let all = combined(&bins, &runs);
        assert!(!all.ok);
        let tests = crate::libtest::failed_tests(&all.output).expect("accounted for");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].spec, "-p x --test b");
        assert_eq!(tests[0].name, "t::broke");
        let findings = crate::findings::test_findings("targo test --tests", &all.output, &[]);
        assert_eq!(findings[0].id, "-p x --test b -- t::broke");
        assert!(
            all.output
                .ends_with("error: 1 target failed:\n    `-p x --test b`\n")
        );
        let a = all.output.find("tests/a.rs").unwrap();
        let b = all.output.find("tests/b.rs").unwrap();
        let c = all.output.find("tests/c.rs").unwrap();
        assert!(a < b && b < c, "declared order");

        let crashed = Run {
            ok: false,
            output: format!("{}\n\nrunning 1 test\n", bins[1].header),
            code: None,
            spawn_error: None,
        };
        let all = combined(&bins, &[pass(&bins[0]), crashed, pass(&bins[2])]);
        assert!(all.output.contains(
            "error: test failed, to rerun pass `-p x --test b`\n\nCaused by:\n  process didn't \
             exit successfully: `/r/target/debug/deps/b-1 -Zunstable-options --report-time` (killed by a signal)\n"
        ));
        assert_eq!(
            crate::libtest::failed_tests(&all.output),
            None,
            "a crash leaves the log unaccounted, as cargo's did: the row is one finding"
        );

        let sentinel = crate::libtest::COULD_NOT_RUN_SENTINEL;
        let refused = Run {
            ok: false,
            output: fail(&bins[1])
                .output
                .replace("assertion failed: false", &format!("{sentinel} — strays")),
            code: Some(101),
            spawn_error: None,
        };
        let all = combined(&bins, &[pass(&bins[0]), refused, pass(&bins[2])]);
        assert!(all.environment_failure().is_some(), "{}", all.output);
        assert!(combined(&bins, &[pass(&bins[0]), pass(&bins[1]), pass(&bins[2])]).ok);
    }

    #[test]
    fn the_replayed_command_is_the_record_with_its_thread_count() {
        let b = Binary {
            header: "     Running tests/a.rs (target/debug/deps/a-1)".into(),
            spec: "-p x --test a".into(),
            record: rec("/r/target/debug/deps/a-1", "x"),
        };
        let cmd = binary_cmd(&b, 7);
        assert_eq!(
            cmd.argv(),
            [
                "/r/target/debug/deps/a-1",
                "-Zunstable-options",
                "--report-time"
            ]
        );
        assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/r/crates/x")));
        assert!(cmd.exact_env);
        assert_eq!(
            cmd.envs
                .iter()
                .filter(|(k, _)| k == "RUST_TEST_THREADS")
                .map(|(_, v)| v.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["7"],
            "one value, the run's — never the recorded 14 beside it"
        );
        assert!(
            cmd.envs
                .iter()
                .any(|(k, v)| k == "CARGO" && v == "/store/bin/targo")
        );
        assert_eq!(
            cmd.preamble.as_deref(),
            Some("     Running tests/a.rs (target/debug/deps/a-1)\n")
        );
        let all = [b.clone(), b.clone(), b];
        let s = summary(&all, Limits::by_load(8, 18), 14, 2, 3);
        assert!(
            s.contains(
                "3 — RUST_TEST_THREADS=14 each, the 1 never timed first, then longest first by \
                 the last run's times, up to 8 at once, past 2 only while fewer than 18 cores \
                 read busy: 3 at the most"
            ),
            "{s}"
        );
        let s = summary(&all, Limits::by_load(1, 18), 14, 0, 1);
        assert!(
            s.ends_with("each, in cargo's order (no times recorded yet), 1 at a time"),
            "{s}"
        );
        let s = summary(&all, Limits::by_load(1, 18), 14, 3, 1);
        assert!(
            s.ends_with("each, longest first by the last run's times, 1 at a time"),
            "{s}"
        );
    }
}
