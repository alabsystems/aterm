// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TEST BINARIES, SEVERAL AT A TIME (2026-09-26).
//!
//! WHY. cargo runs test binaries one at a time, even under `--no-fail-fast`, so
//! the test stage's run was a serial walk over ~400 binaries on a 14-core
//! machine: the two ladder logs the push-gate audit measured spent 1224.7 s and
//! 1421 s inside test binaries (the sum of each one's `finished in`), 287 of
//! them under a second each, and the stage sat on the ladder's critical path.
//! This module runs them `--test-jobs` at a time ([`DEFAULT_JOBS`], with
//! `RUST_TEST_THREADS` split between them), and the few that cannot share the
//! machine with another test binary alone ([`EXCLUSIVE_TEST_BINARIES`]).
//!
//! THE PROCESS IS CARGO'S, NOT A GUESS AT IT. A test binary is not just a path:
//! cargo starts it in its package's root with an environment of its own —
//! `CARGO` (under targo, targo itself), `CARGO_MANIFEST_DIR`, the whole
//! `CARGO_PKG_*` family, the dynamic-library search path, every
//! `cargo:rustc-env` its build script printed, targo's own
//! `TRUST_TARGO_FRONTEND` — and tests in this tree read several of those at
//! run time (`CARGO` in trust-gate, aterm-gui, aterm-forge and the conformance
//! support). Rebuilding that from the compile's JSON would be a copy of cargo
//! that drifts. So cargo is asked: the stage runs the very `targo test --tests`
//! it always ran, with one addition — `--config
//! target.'cfg(all())'.runner=[<this binary>, --record-test-binary, <dir>]` —
//! and cargo, instead of executing each test binary, executes this binary with
//! the test binary's argv, in its directory, with its environment.
//! [`record_main`] writes those three facts down and exits 0, so cargo walks
//! every binary in a few seconds and runs nothing; the gate then runs the
//! recorded invocations itself.
//!
//! WHAT ELSE IT READS. The compile child (`--no-run`) is asked for
//! `--message-format=json-render-diagnostics`: its diagnostics print exactly as
//! before, and each `compiler-artifact` line names a test executable and its
//! target's kind and name — which, with the recorded `CARGO_PKG_NAME`, is
//! cargo's own re-run spec (`-p atpkg --test index_probe`, `-p aterm-gui
//! --lib`), the id every failed test is known by in a receipt
//! ([`crate::differential::test_findings`]). The JSON lines themselves are
//! removed from what the ladder shows.
//!
//! THE LOG IS CARGO'S SHAPE. Each binary's output is captured on its own and
//! printed in cargo's order — its `Running` header, its bytes, and, when it
//! failed, `error: test failed, to rerun pass \`<spec>\`` — so the ladder reads
//! exactly as a serial run's would, and every reader of that log
//! ([`crate::libtest::failed_tests`], [`crate::libtest::environment_refusals`],
//! the differential's fingerprints) reads it unchanged. The header is written
//! at the head of each binary's own log before it starts
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

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::exec::{self, Cmd, Run};
use crate::ladder::Report;

/// The flag cargo's runner config hands this binary: `aterm-verify
/// --record-test-binary <dir> <test binary> <its args…>` ([`record_main`]).
pub const RECORD_FLAG: &str = "--record-test-binary";

/// How many test binaries run at once unless `--test-jobs` says otherwise. Two
/// is where the audit said to START (2026-09-26): each gets half the pinned
/// `RUST_TEST_THREADS`, so the run offers the machine the same threads it did,
/// and a binary that idles on a subprocess, a socket or a sleep no longer idles
/// the stage. Raise it from measurements, never from hope.
///
/// MEASURED 2026-09-26 (`measure_the_test_run_on_this_tree`, six crates, 147
/// binaries, one-minute load 37-84 on 14 cores): two at a time, the binaries'
/// wall was 315.3 s, 323.8 s and 326.1 s over three runs, against 533.5 s,
/// 559.8 s and 576.9 s for the same binaries' own durations summed — what one
/// at a time costs at the least — and cargo's serial `--tests` child, run
/// between them, took 517.5 s. (Those runs' stage totals, 547-579 s, also hold
/// a 229-253 s compile, which the serial figure does not separate out.)
pub const DEFAULT_JOBS: u32 = 2;

/// THE BINARIES THAT RUN ALONE: a re-run spec prefix (one ending in a space
/// covers every target it begins; otherwise the spec must match exactly), and
/// why no other test binary may run beside it. They run first, one at a time,
/// with the whole `RUST_TEST_THREADS`, before any shared slot opens.
///
/// ADMISSION IS A MECHANISM, NOT A HUNCH: a binary joins when something it does
/// is decided by another process on the machine. Checked and left off
/// (2026-09-26), with why:
///
/// * the flock and sweeper tests the audit named (`atpkg` index_probe,
///   `aterm-agent`'s `a_second_sweeper_does_nothing_while_the_first_holds_the_lock`)
///   lock files under per-test, per-pid scratch directories, and their reds of
///   2026-09-23 were an in-process defect — a lock released by `close` while a
///   thread's `posix_spawn` child still held the description — fixed by
///   54ec81ca4 / 773b7136f. Nothing another binary does reaches those files.
/// * fixed ports and paths: no test binds a fixed port (every listener binds
///   port 0), and the scratch roots found key on the test process's pid.
///
/// * aterm-conformance's paint and spin suites (listed until 2026-09-27): every
///   test in them is under `measuring::`, which this run skips — the MEASURE
///   tier's `measuring tests` stage runs them, alone — so here they run no
///   test, and alone they bought nothing.
pub const EXCLUSIVE_TEST_BINARIES: [(&str, &str); 1] = [(
    "-p aterm-link --test ",
    "the world harness refuses a machine where an `aterm-gui --headless` or \
     `aterm-link serve` older than its own process is alive (`strays_now` in \
     tests/harness/mod.rs), and any binary beside it — another world's, or one \
     that starts a headless aterm — is exactly that",
)];

/// Why the binary with re-run spec `spec` must run alone, if it must.
#[must_use]
pub fn exclusive_reason(spec: &str) -> Option<&'static str> {
    EXCLUSIVE_TEST_BINARIES
        .iter()
        .find(|(pat, _)| spec == *pat || (pat.ends_with(' ') && spec.starts_with(pat)))
        .map(|(_, why)| *why)
}

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
        let Some(v) = json::parse(t) else {
            bad.get_or_insert_with(|| t.chars().take(120).collect::<String>());
            continue;
        };
        if v.get("reason").and_then(json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let test = v
            .get("profile")
            .and_then(|p| p.get("test"))
            .and_then(json::Value::as_bool)
            == Some(true);
        let Some(exe) = v.get("executable").and_then(json::Value::as_str) else {
            continue;
        };
        if !test {
            continue;
        }
        let target = v.get("target");
        let kinds: Vec<&str> = target
            .and_then(|t| t.get("kind"))
            .and_then(json::Value::as_array)
            .map(|a| a.iter().filter_map(json::Value::as_str).collect())
            .unwrap_or_default();
        let name = target
            .and_then(|t| t.get("name"))
            .and_then(json::Value::as_str)
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

/// One test binary the gate will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    /// cargo's `     Running <target> (<path>)` line for it, verbatim.
    pub header: String,
    /// cargo's re-run spec: `-p <package> <flag>`.
    pub spec: String,
    pub record: Record,
    /// Why it runs alone ([`EXCLUSIVE_TEST_BINARIES`]), when it does.
    pub alone: Option<&'static str>,
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
                alone: exclusive_reason(&spec),
                spec,
                record,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Running them.
// ---------------------------------------------------------------------------

/// Run every binary: the [`Binary::alone`] ones first, one at a time, with
/// `threads`; then the rest `jobs` at a time in declared order, each with
/// `threads / jobs` (at least one). `run_one(binary, its threads)` runs one;
/// the answers come back in declared order.
pub fn run_all<F>(bins: &[Binary], jobs: usize, threads: u32, run_one: F) -> Vec<Run>
where
    F: Fn(&Binary, u32) -> Run + Sync,
{
    let slots: Vec<Mutex<Option<Run>>> = bins.iter().map(|_| Mutex::new(None)).collect();
    for (i, b) in bins.iter().enumerate() {
        if b.alone.is_some() {
            *slots[i].lock().expect("slot") = Some(run_one(b, threads));
        }
    }
    let shared: Vec<usize> = (0..bins.len())
        .filter(|&i| bins[i].alone.is_none())
        .collect();
    let jobs = jobs.clamp(1, shared.len().max(1));
    let each = shared_threads(threads, jobs);
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&i) = shared.get(k) else { break };
                    let run = run_one(&bins[i], each);
                    *slots[i].lock().expect("slot") = Some(run);
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|s| {
            s.into_inner().ok().flatten().unwrap_or_else(|| Run {
                ok: false,
                output: "aterm-verify: this test binary was never run (a gate defect)\n".into(),
                code: None,
                spawn_error: None,
            })
        })
        .collect()
}

/// `RUST_TEST_THREADS` for a binary sharing the machine with `jobs - 1` others.
#[must_use]
pub fn shared_threads(threads: u32, jobs: usize) -> u32 {
    let jobs = u32::try_from(jobs.max(1)).unwrap_or(u32::MAX);
    (threads / jobs).max(1)
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

/// The ladder's one line about how the binaries ran.
#[must_use]
pub fn summary(bins: &[Binary], jobs: usize, threads: u32) -> String {
    let alone: Vec<&str> = bins
        .iter()
        .filter(|b| b.alone.is_some())
        .map(|b| b.spec.as_str())
        .collect();
    let shared = bins.len() - alone.len();
    let jobs = jobs.clamp(1, shared.max(1));
    let mut s = format!(
        "  test binaries: {} — cargo recorded each one's argv, directory and environment \
         (a runner in its place) and the gate ran them: ",
        bins.len()
    );
    if !alone.is_empty() {
        s.push_str(&format!(
            "{} alone first (RUST_TEST_THREADS={threads}: {}), then ",
            alone.len(),
            alone.join(", ")
        ));
    }
    s.push_str(&format!(
        "{shared} shared, {jobs} at a time (RUST_TEST_THREADS={} each)",
        shared_threads(threads, jobs)
    ));
    s
}

/// A binary the test run's ceiling left no time to start: a FAIL in cargo's
/// shape (its header, then why), never a pass and never a skip, and — it says
/// [`crate::differential::CEILING_KILL`] — never inherited.
#[must_use]
pub fn not_started(b: &Binary, limit: std::time::Duration) -> Run {
    Run {
        ok: false,
        output: format!(
            "{}\n{} — not started: the test run's {:.1}s wall-clock ceiling was spent before \
             this binary's turn\n\x20 test binary: {}\n\x20 It decided NOTHING: a run that \
             never finishes is a FAIL, never a pass and never a skip.\n\x20 Raise the ceiling \
             with --stage-timeout <seconds>, or remove it with --stage-timeout off.\n",
            b.header,
            crate::differential::CEILING_KILL,
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
    let serially = |r: &mut Report, why: &str| {
        r.raw(format!(
            "  test binaries: run by cargo, one at a time — {why}"
        ));
        let out = exec::run(serial, ctx.exec_env());
        r.raw(out.output.as_str());
        r.decide_test_child(&out, label);
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
        serially(r, "the gate could not name itself as the recording runner");
        return;
    };
    let pass = exec::run(&recording_cmd(serial, &config), ctx.exec_env());
    let records = read_records(&dir);
    std::fs::remove_dir_all(&dir).ok();

    // RECORDS THAT CANNOT BE READ ARE NEVER "NO RECORDS" (2026-09-27): the
    // pass behind them may have executed nothing — every binary replaced by a
    // recorder that exited 0 — and judged as the serial run it was not, it
    // would be an `ok` row with no test run. The serial child decides instead.
    if let Err(why) = &records {
        serially(r, &format!("the recording could not be read: {why}"));
        return;
    }
    // The recorder ran if it recorded something, or if cargo reports it
    // failing (`process didn't exit successfully: \`<gate> --record-test-binary
    // …\``): a recorder that could not write is the gate's failure, never the
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
        r.decide_test_child(&pass, label);
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
    let jobs = ctx.test_jobs();
    let threads = ctx.test_threads();
    // ONE CEILING FOR THE WHOLE RUN (2026-09-27), as the serial child had: a
    // binary is started with what is left of it, and one whose turn comes
    // after it is spent is not started. Each binary's own full ceiling left the
    // stage with no bound at all — two hangs, two ceilings.
    let ceiling = ctx.exec_env().child_ceiling;
    let started = std::time::Instant::now();
    let runs = run_all(&bins, jobs, threads, |b, t| {
        let mut env = ctx.exec_env();
        if let Some(limit) = ceiling {
            let left = limit.saturating_sub(started.elapsed());
            if left.is_zero() {
                return not_started(b, limit);
            }
            env.child_ceiling = Some(left);
        }
        exec::run(&binary_cmd(b, t), env)
    });
    // cargo's own lines from the pass (its warnings, `Finished`), without the
    // headers the binaries' logs carry.
    let preface: String = pass
        .output
        .lines()
        .filter(|l| !l.trim_start().starts_with("Running "))
        .map(|l| format!("{l}\n"))
        .collect();
    r.raw(preface);
    r.raw(summary(&bins, jobs, threads));
    let all = combined(&bins, &runs);
    r.raw(all.output.as_str());
    r.decide_test_child(&all, label);
}

static RECORDING: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// A JSON reader for cargo's messages. This crate has no dependencies on purpose
// (Cargo.toml), and cargo's messages are one object per line.
// ---------------------------------------------------------------------------

pub mod json {
    /// A JSON value; numbers keep their text.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Value {
        Null,
        Bool(bool),
        Num(String),
        Str(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    impl Value {
        /// A member of an object.
        #[must_use]
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Object(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        #[must_use]
        pub fn as_str(&self) -> Option<&str> {
            match self {
                Value::Str(s) => Some(s),
                _ => None,
            }
        }

        #[must_use]
        pub fn as_bool(&self) -> Option<bool> {
            match self {
                Value::Bool(b) => Some(*b),
                _ => None,
            }
        }

        #[must_use]
        pub fn as_array(&self) -> Option<&[Value]> {
            match self {
                Value::Array(a) => Some(a),
                _ => None,
            }
        }
    }

    /// One value filling `text` (surrounding whitespace allowed), or `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Value> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
            depth: 0,
        };
        let v = p.value()?;
        p.ws();
        (p.i == p.b.len()).then_some(v)
    }

    struct Parser<'a> {
        b: &'a [u8],
        i: usize,
        depth: usize,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while self.b.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
                self.i += 1;
            }
        }

        fn eat(&mut self, c: u8) -> Option<()> {
            self.ws();
            (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
        }

        fn lit(&mut self, word: &str, v: Value) -> Option<Value> {
            self.b[self.i..].starts_with(word.as_bytes()).then(|| {
                self.i += word.len();
                v
            })
        }

        fn value(&mut self) -> Option<Value> {
            self.ws();
            self.depth += 1;
            if self.depth > 256 {
                return None;
            }
            let v = match *self.b.get(self.i)? {
                b'{' => self.object(),
                b'[' => self.array(),
                b'"' => self.string().map(Value::Str),
                b't' => self.lit("true", Value::Bool(true)),
                b'f' => self.lit("false", Value::Bool(false)),
                b'n' => self.lit("null", Value::Null),
                b'-' | b'0'..=b'9' => self.number(),
                _ => None,
            };
            self.depth -= 1;
            v
        }

        fn object(&mut self) -> Option<Value> {
            self.i += 1;
            let mut m = Vec::new();
            if self.eat(b'}').is_some() {
                return Some(Value::Object(m));
            }
            loop {
                self.ws();
                let k = self.string()?;
                self.eat(b':')?;
                m.push((k, self.value()?));
                if self.eat(b',').is_some() {
                    continue;
                }
                self.eat(b'}')?;
                return Some(Value::Object(m));
            }
        }

        fn array(&mut self) -> Option<Value> {
            self.i += 1;
            let mut a = Vec::new();
            if self.eat(b']').is_some() {
                return Some(Value::Array(a));
            }
            loop {
                a.push(self.value()?);
                if self.eat(b',').is_some() {
                    continue;
                }
                self.eat(b']')?;
                return Some(Value::Array(a));
            }
        }

        fn number(&mut self) -> Option<Value> {
            let start = self.i;
            while self
                .b
                .get(self.i)
                .is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c))
            {
                self.i += 1;
            }
            let text = std::str::from_utf8(&self.b[start..self.i]).ok()?;
            text.parse::<f64>().ok()?;
            Some(Value::Num(text.to_string()))
        }

        fn hex4(&mut self) -> Option<u32> {
            let h = self.b.get(self.i..self.i + 4)?;
            self.i += 4;
            u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
        }

        fn string(&mut self) -> Option<String> {
            if self.b.get(self.i) != Some(&b'"') {
                return None;
            }
            self.i += 1;
            let mut out = Vec::new();
            loop {
                let c = *self.b.get(self.i)?;
                self.i += 1;
                match c {
                    b'"' => return String::from_utf8(out).ok(),
                    b'\\' => {
                        let e = *self.b.get(self.i)?;
                        self.i += 1;
                        let ch = match e {
                            b'"' => '"',
                            b'\\' => '\\',
                            b'/' => '/',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            b'u' => {
                                let hi = self.hex4()?;
                                let code = if (0xD800..0xDC00).contains(&hi) {
                                    if self.b.get(self.i..self.i + 2)? != b"\\u" {
                                        return None;
                                    }
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&lo) {
                                        return None;
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    hi
                                };
                                char::from_u32(code)?
                            }
                            _ => return None,
                        };
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    c if c < 0x20 => return None,
                    c => out.push(c),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(exe: &str, pkg: &str) -> Record {
        Record {
            cwd: PathBuf::from(format!("/r/crates/{pkg}")),
            argv: vec![exe.into(), "--skip".into(), "measuring::".into()],
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
            "--skip",
            "measuring::",
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
                "--skip",
                "measuring::"
            ],
            "cargo's flag before the separator, libtest's arguments untouched after it"
        );
    }

    #[test]
    fn the_json_reader_reads_cargos_messages_and_refuses_the_rest() {
        let v = json::parse(r#" {"a":[1,-2.5e3,true,null,"x\"\\é😀"],"b":{}} "#).expect("json");
        assert_eq!(
            v.get("a").and_then(json::Value::as_array).map(<[_]>::len),
            Some(5)
        );
        assert_eq!(
            v.get("a")
                .and_then(|a| a.as_array())
                .and_then(|a| a[4].as_str()),
            Some("x\"\\é😀")
        );
        for bad in [
            "{",
            r#"{"a":}"#,
            r#"{"a":1,}"#,
            "[1 2]",
            r#""\ud83d""#,
            "{} x",
            "tru",
        ] {
            assert_eq!(json::parse(bad), None, "{bad}");
        }
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
    /// is cargo's, and the binaries that must run alone are marked.
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
        assert!(bins[0].alone.is_none(), "the lib spawns no world");
        assert!(bins[1].alone.is_some(), "a world-harness binary runs alone");

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

    #[test]
    fn the_exclusive_list_matches_whole_specs_or_declared_prefixes() {
        assert!(exclusive_reason("-p aterm-link --test r13_presence").is_some());
        assert!(exclusive_reason("-p aterm-link --lib").is_none());
        assert!(exclusive_reason("-p aterm-linker --test x").is_none());
        assert!(exclusive_reason("-p aterm-conformance --test paint").is_none());
    }

    /// EVERY DECLARED ENTRY NAMES SOMETHING IN THIS TREE, so the list cannot
    /// rot into names that exempt nothing: a prefix entry's package has
    /// integration tests, an exact entry's test file exists.
    #[test]
    fn every_exclusive_entry_names_a_target_in_this_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (pat, why) in EXCLUSIVE_TEST_BINARIES {
            assert!(!why.is_empty());
            let words: Vec<&str> = pat.split_whitespace().collect();
            let ["-p", package, "--test", rest @ ..] = words.as_slice() else {
                panic!("an entry is `-p <package> --test [<name>]`: {pat:?}");
            };
            let tests = root.join("crates").join(package).join("tests");
            match rest {
                [] => assert!(
                    std::fs::read_dir(&tests)
                        .map(|d| d
                            .filter_map(Result::ok)
                            .any(|e| { e.path().extension().is_some_and(|x| x == "rs") }))
                        .unwrap_or(false),
                    "{pat:?}: {} holds no integration test",
                    tests.display()
                ),
                [name] => assert!(
                    tests.join(format!("{name}.rs")).is_file()
                        || tests.join(name).join("main.rs").is_file(),
                    "{pat:?}: no such test target"
                ),
                _ => panic!("{pat:?}"),
            }
        }
    }

    /// THE SCHEDULE: the lone binaries first, each with nothing else running and
    /// the whole thread count; the rest never more than `jobs` at once, each
    /// with its share; and the answers in declared order whatever order they
    /// finished in.
    #[test]
    fn alone_ones_run_alone_first_and_the_rest_share_in_declared_order() {
        let bins: Vec<Binary> = (0..9)
            .map(|i| Binary {
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
                alone: (i == 3 || i == 7).then_some("because"),
            })
            .collect();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let order = Mutex::new(Vec::new());
        let runs = run_all(&bins, 3, 12, |b, threads| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            if b.alone.is_some() {
                assert_eq!(now, 1, "{} ran beside another binary", b.spec);
                assert_eq!(threads, 12);
            } else {
                assert_eq!(threads, 4, "12 threads over 3 jobs");
            }
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
        });
        let order = order.into_inner().unwrap();
        assert_eq!(
            &order[..2],
            ["-p x --test t3", "-p x --test t7"],
            "{order:?}"
        );
        assert!(peak.load(Ordering::SeqCst) <= 3);
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
        assert_eq!(shared_threads(14, 2), 7);
        assert_eq!(shared_threads(3, 8), 1, "never zero threads");
    }

    /// A BINARY THE RUN'S CEILING LEFT NO TIME TO START (2026-09-27) is a FAIL
    /// in cargo's shape — its header, the gate's TIMEOUT words, cargo's re-run
    /// line — and its row is never inherited.
    #[test]
    fn a_binary_the_ceiling_left_no_time_for_is_a_fail_never_inherited() {
        let b = Binary {
            header: "     Running tests/late.rs (target/debug/deps/late-1)".into(),
            spec: "-p x --test late".into(),
            record: rec("/r/late", "x"),
            alone: None,
        };
        let run = not_started(&b, std::time::Duration::from_secs(10_800));
        assert!(!run.ok);
        assert!(run.output.starts_with(&b.header), "{}", run.output);
        assert!(
            run.output.contains(crate::differential::CEILING_KILL),
            "{}",
            run.output
        );
        let all = combined(std::slice::from_ref(&b), std::slice::from_ref(&run));
        assert!(!all.ok);
        let found = crate::differential::test_findings("targo test --tests", &all.output);
        assert_eq!(found.len(), 1);
        assert!(found[0].opaque.is_some(), "{found:?}");
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
                alone: None,
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
        let findings = crate::differential::test_findings("targo test --tests", &all.output);
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
             exit successfully: `/r/target/debug/deps/b-1 --skip measuring::` (killed by a signal)\n"
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
    fn the_replayed_command_is_the_record_with_its_share_of_threads() {
        let b = Binary {
            header: "     Running tests/a.rs (target/debug/deps/a-1)".into(),
            spec: "-p x --test a".into(),
            record: rec("/r/target/debug/deps/a-1", "x"),
            alone: None,
        };
        let cmd = binary_cmd(&b, 7);
        assert_eq!(
            cmd.argv(),
            ["/r/target/debug/deps/a-1", "--skip", "measuring::"]
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
            "one value, the share — never the recorded 14 beside it"
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
        let lone = Binary {
            alone: Some("x"),
            spec: "-p l --test w".into(),
            ..b.clone()
        };
        let s = summary(&[b.clone(), lone, b.clone(), b], 2, 14);
        assert!(
            s.contains(
                "4 — cargo recorded each one's argv, directory and environment (a runner in its \
                 place) and the gate ran them: 1 alone first (RUST_TEST_THREADS=14: -p l --test \
                 w), then 3 shared, 2 at a time (RUST_TEST_THREADS=7 each)"
            ),
            "{s}"
        );
    }
}
