// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running test binaries concurrently.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::exec::{self, Cmd, Run};
use crate::ladder::Report;

/// The flag cargo's runner config hands this binary.
pub const RECORD_FLAG: &str = "--record-test-binary";

/// How many test binaries run at once unless `--test-jobs` says otherwise.
pub const DEFAULT_JOBS: u32 = 8;

const RECORD_MAGIC: &[u8] = b"aterm-verify test record 1\0";

/// One test binary as cargo would have started it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The directory cargo starts it in.
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
    /// This process, as cargo started it.
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

    /// NUL-separated tokens after a magic line.
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

    /// [`Record::encode`] read back.
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

/// `aterm-verify --record-test-binary <dir> <test binary> <args…>`.
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

/// The `--config` value that makes cargo run `gate --record-test-binary dir` in place of every test binary.
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

/// Build the serial `--tests` command with the recording runner prepended before the subcommand alongside [`crate::stages::main_targo`].
#[must_use]
pub fn recording_cmd(serial: &Cmd, config: &str) -> Cmd {
    let mut c = serial.clone();
    c.args
        .splice(0..0, [OsString::from("--config"), OsString::from(config)]);
    c
}

/// A test executable the compile built, with cargo's target flag for it (`--lib`, `--bin <name>`, `--test <name>`, …).
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

/// Take cargo's JSON messages out of a `--message-format=json-render-diagnostics` log, in place.
///
/// # Errors
/// A message that does not parse.
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

/// One test binary the run will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    /// cargo's ` Running <target> (<path>)` line for it, verbatim.
    pub header: String,
    /// cargo's re-run spec.
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

/// Pair each record with the header cargo printed for it and the compile's flag for its executable.
///
/// # Errors
/// Whatever does not add up, in a few words.
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
                .find(|e| e.exe.file_name().is_some() && e.exe.file_name() == exe.file_name())
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

/// Run every binary through [`pool`], `jobs` at a time.
pub fn run_all<F>(bins: &[Binary], jobs: usize, threads: u32, run_one: F) -> Vec<Run>
where
    F: Fn(&Binary, u32) -> Run + Sync,
{
    let all: Vec<usize> = (0..bins.len()).collect();
    pool(bins.len(), &all, jobs, |i| run_one(&bins[i], threads))
        .into_iter()
        .map(|pooled| {
            pooled.unwrap_or_else(|| Run {
                ok: false,
                output: "aterm-verify: this test binary was never run (a gate defect)\n".into(),
                code: None,
                spawn_error: None,
            })
        })
        .collect()
}

/// Run `run(i)` for each distinct index below `n` in `order`, `jobs` at a time (at least one).
pub fn pool<T, F>(n: usize, order: &[usize], jobs: usize, run: F) -> Vec<Option<T>>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    let answers: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, order.len().max(1)) {
            scope.spawn(|| {
                while let Some(&i) = order.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let out = run(i);
                    *answers[i]
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(out);
                }
            });
        }
    });
    answers
        .into_iter()
        .map(|s| s.into_inner().ok().flatten())
        .collect()
}

/// The command that runs `b` as cargo would have, with `threads` test threads and its `Running` header at the head of its log.
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

/// The one log cargo would have written for these runs, in declared order.
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
    let n = bins.len();
    format!(
        "  test binaries: {n} — RUST_TEST_THREADS={threads} each, {} at a time",
        jobs.min(n.max(1))
    )
}

/// A binary the test run's ceiling left no time to start.
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

/// Decide the test run's row exactly as the serial child's was decided.
fn decide(r: &mut Report, run: &Run, label: &str, env: &[(OsString, OsString)]) {
    r.decide_test_child(run, label, env);
}

/// THE TEST RUN (the test stage's second half, after its compile).
pub fn run_tests(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    serial: &Cmd,
    exes: Result<Vec<TestExe>, String>,
) {
    #[cfg(unix)]
    match aterm_uds::host::open_file_room(4096) {
        Ok(room) => {
            r.raw(format!(
                "  test descriptors: soft {} -> {}, hard {}, target {} (requested 4096)",
                room.before, room.after, room.hard, room.target
            ));
            if let Some(error) = room.refusal {
                r.raw(format!("  test descriptor raise refused: {error}"));
            }
        }
        Err(error) => r.raw(format!("  test descriptor room unavailable: {error}")),
    }
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

    if let Err(why) = &records {
        serially(r, &format!("the recording could not be read: {why}"));
        return;
    }
    let recorded = records.as_ref().is_ok_and(|v| !v.is_empty());
    let recorder_failed = pass
        .output
        .lines()
        .any(|l| l.contains("didn't exit successfully") && l.contains(RECORD_FLAG));
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
    let jobs = ctx.test_jobs();
    let threads = ctx.test_threads();
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
    decide(r, &all, label, &env);
}

static RECORDING: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(exe: &str, pkg: &str) -> Record {
        Record {
            cwd: PathBuf::from(format!("/r/crates/{pkg}")),
            argv: vec![exe.into(), "--skip".into(), "slow::".into()],
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
            "--config",
            "CACHE",
            "--unverified",
            "test",
            "--tests",
            "--",
            "--skip",
            "x",
        ]);
        assert_eq!(
            recording_cmd(&serial, "CFG").argv(),
            [
                "targo",
                "--config",
                "CFG",
                "--config",
                "CACHE",
                "--unverified",
                "test",
                "--tests",
                "--",
                "--skip",
                "x"
            ],
            "the runner before the subcommand, beside the build cache's --config (one after \
             it would replace the cache's); libtest's arguments untouched"
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
        let in_build_dir: Vec<TestExe> = exes
            .iter()
            .map(|e| TestExe {
                exe: Path::new("/cache/r/.cargo-build/debug/deps")
                    .join(e.exe.file_name().expect("a file name")),
                flag: e.flag.clone(),
            })
            .collect();
        assert_eq!(plan(recs(), log, &in_build_dir).expect("a plan"), bins);
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

    /// THE POOL: never more than `jobs` at once, every binary once and with the
    /// whole thread count, as cargo's own run gives it, and the answers in
    /// declared order whatever order they finished in.
    #[test]
    fn binaries_share_the_pool_and_answer_in_declared_order() {
        let bins: Vec<Binary> = (0..9)
            .map(|i| Binary {
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let runs = run_all(&bins, 3, 12, |b, threads| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            assert_eq!(threads, 12, "every binary gets the whole count");
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
        assert!(peak.load(Ordering::SeqCst) <= 3, "never past the jobs");
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
             exit successfully: `/r/target/debug/deps/b-1 --skip slow::` (killed by a signal)\n"
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
        let why = all
            .environment_failure()
            .expect("the machine's, not a finding");
        assert!(
            why.ends_with("— strays"),
            "the row names the refusal's why: {why}"
        );
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
        assert_eq!(cmd.argv(), ["/r/target/debug/deps/a-1", "--skip", "slow::"]);
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
    }
}
