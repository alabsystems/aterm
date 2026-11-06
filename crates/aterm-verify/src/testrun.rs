// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running test binaries concurrently.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::exec::{self, Cmd, Run};
use crate::ladder::{Outcome, Report, Severity};

/// The flag cargo's runner config hands this binary.
pub const RECORD_FLAG: &str = "--record-test-binary";

/// How many test binaries run at once unless `--test-jobs` says otherwise.
pub const DEFAULT_JOBS: u32 = 8;

/// Environment variable naming the shared subprocess slot directory created by [`run_tests`].
pub const SUBPROCESS_SLOTS_ENV: &str = "ATERM_TEST_SUBPROCESS_SLOTS";

/// Compute the number of concurrent multi-process test stacks admitted for `cores` granted CPUs.
#[must_use]
pub const fn subprocess_slots_for_cores(cores: u32) -> usize {
    if cores <= 1 {
        1
    } else {
        let half = (cores as usize) / 2;
        if half < 2 {
            2
        } else if half > 8 {
            8
        } else {
            half
        }
    }
}

/// Compute the number of concurrent process-tree boots admitted for `slots` live stack slots.
#[must_use]
pub const fn subprocess_boot_slots(slots: usize) -> usize {
    let raw = slots / 2;
    if raw < 1 {
        1
    } else if raw > 3 {
        3
    } else {
        raw
    }
}

fn init_subprocess_pool_dir(dir: &Path, slots: usize) -> std::io::Result<()> {
    use std::io::Write as _;

    let slots = slots.clamp(1, 64);
    let boots = subprocess_boot_slots(slots);
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("subprocess pool is not a directory: {}", dir.display()),
        ));
    }
    #[cfg(unix)]
    {
        crate::set_dir_mode_nofollow(dir, 0o700)?;
    }
    let create = |path: PathBuf, contents: Option<usize>| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        let mut file = opts.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        if let Some(count) = contents {
            writeln!(file, "{count}")?;
        }
        Ok(())
    };
    (|| {
        for idx in 0..slots {
            create(dir.join(format!("slot-{idx:04}.lock")), None)?;
        }
        for idx in 0..boots {
            create(dir.join(format!("boot-{idx:04}.lock")), None)?;
        }
        create(dir.join("count"), Some(slots))?;
        create(dir.join("boot_count"), Some(boots))?;
        Ok(())
    })()
}

fn subprocess_pool(scratch: &Path, slots: usize) -> std::io::Result<aterm_tempfile::TempDir> {
    std::fs::create_dir_all(scratch)?;
    let pool = aterm_tempfile::Builder::new()
        .prefix("test-subprocs.")
        .suffix(".noindex")
        .tempdir_in(scratch)?;
    init_subprocess_pool_dir(pool.path(), slots)?;
    Ok(pool)
}

/// How many of the run's slowest binaries [`poles`] names.
const POLES_SHOWN: usize = 8;

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

    /// The value of `key` in the recorded environment, its last setting winning. On Windows a
    /// name is found without regard to ASCII case, as that environment answers it: a test whose
    /// `PATH` cargo set still holds it under the `Path` it inherited.
    #[must_use]
    pub fn var(&self, key: &str) -> Option<&OsStr> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == key || (cfg!(windows) && k.eq_ignore_ascii_case(key)))
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
                b"cwd" if cwd.is_none() => cwd = Some(PathBuf::from(os_of(it.next()?))),
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
            open.mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
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
    let mut names: Vec<PathBuf> = Vec::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("cannot read entry in {}: {e}", dir.display()))?;
        let p = entry.path();
        if p.extension().is_some_and(|x| x == "rec") {
            names.push(p);
        }
    }
    names.sort();
    names
        .iter()
        .map(|p| {
            let bytes =
                crate::read_regular(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
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

/// Build the serial `--tests` command with the recording runner and current target metadata.
#[must_use]
pub fn recording_cmd(serial: &Cmd, config: &str) -> Cmd {
    let mut c = crate::compile_account::with_message_format(serial.clone());
    c.args
        .splice(0..0, [OsString::from("--config"), OsString::from(config)]);
    c.args = aterm_toolchain::build_cache::configs_after_subcommand(&c.args);
    c
}

/// A test executable the compile built, with cargo's target flag for it (`--lib`, `--bin <name>`, `--test <name>`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestExe {
    pub exe: PathBuf,
    pub flag: String,
    /// `CARGO_BIN_EXE_<name>` for each binary of its package the compile reported. Cargo names
    /// them only while it compiles a test, so the run hands them over (`aterm_tempfile::bin_exe`
    /// reads them).
    pub bin_env: Vec<(OsString, OsString)>,
    /// Its manifest says it runs libtest's harness ([`harness_of`]), so libtest's flags reach a
    /// libtest `main` and never a custom one.
    pub harness: bool,
}

/// Whether the target `name` of `kinds` runs libtest's harness, as `manifest` (its package's
/// parsed `Cargo.toml`, `None` where it could not be read) declares it: the `harness` of its
/// `[lib]`, or of the one `[[test]]`, `[[bench]]`, `[[bin]]` or `[[example]]` entry of its name
/// (an entry with no `name` is named by its `path`'s stem), true where none declares it (an
/// autodiscovered target). An unreadable manifest, or a name two entries share, is false.
fn harness_of(manifest: Option<&aterm_toml::Value>, kinds: &[&str], name: &str) -> bool {
    use aterm_toml::Value;
    const LIB: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];
    let Some(manifest) = manifest else {
        return false;
    };
    // An entry's own word: true where it says nothing, false where it says something not a bool.
    let declared = |entry: &Value| entry.get("harness").map_or(Some(true), Value::as_bool);
    let said = if kinds.iter().any(|k| LIB.contains(k)) {
        manifest.get("lib").map(declared)
    } else {
        let Some(table) = kinds.first().and_then(|k| match *k {
            "test" | "bench" | "bin" | "example" => Some(*k),
            _ => None,
        }) else {
            return false;
        };
        let entries: Vec<&Value> = manifest
            .get(table)
            .and_then(Value::as_array)
            .map(|all| {
                all.iter()
                    .filter(|e| {
                        e.get("name").and_then(Value::as_str).or_else(|| {
                            e.get("path")
                                .and_then(Value::as_str)
                                .and_then(|p| Path::new(p).file_stem()?.to_str())
                        }) == Some(name)
                    })
                    .collect()
            })
            .unwrap_or_default();
        match entries.as_slice() {
            [] => None,
            [one] => Some(declared(one)),
            _ => return false,
        }
    };
    said.is_none_or(|harness| harness.unwrap_or(false))
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
/// Each test executable carries its package's binaries ([`TestExe::bin_env`]).
///
/// # Errors
/// A message that does not parse.
pub fn take_test_executables(log: &mut String) -> Result<Vec<TestExe>, String> {
    let mut exes = Vec::new();
    let mut bins: Vec<(String, OsString, OsString)> = Vec::new();
    let mut manifests: BTreeMap<String, Option<aterm_toml::Value>> = BTreeMap::new();
    let mut bad = None;
    let mut kept = String::with_capacity(log.len());
    for line in log.split_inclusive('\n') {
        let t = line.trim_end_matches(['\n', '\r']);
        let Ok(message) = aterm_json::from_str::<crate::compiler_message::UnitMessage>(t) else {
            if t.starts_with("{\"reason\":") {
                bad.get_or_insert_with(|| t.chars().take(120).collect::<String>());
            } else {
                kept.push_str(line);
            }
            continue;
        };
        let v = match message {
            crate::compiler_message::UnitMessage::Unit(value) => value,
            crate::compiler_message::UnitMessage::Other => {
                if !t.starts_with("{\"reason\":") {
                    kept.push_str(line);
                }
                continue;
            }
            crate::compiler_message::UnitMessage::Ambiguous(_) => {
                bad.get_or_insert_with(|| "ambiguous compiler message fields".into());
                continue;
            }
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
        let package = v.get("package_id").and_then(aterm_json::Value::as_str);
        let name = v
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(aterm_json::Value::as_str)
            .unwrap_or_default();
        if !test {
            let bin = crate::compiler_message::artifact_target_kinds(&v)
                .is_ok_and(|kinds| kinds == ["bin"]);
            if let (true, Some(package), false) = (bin, package, name.is_empty()) {
                bins.push((
                    package.to_owned(),
                    format!("CARGO_BIN_EXE_{name}").into(),
                    exe.into(),
                ));
            }
            continue;
        }
        let kinds = match crate::compiler_message::artifact_target_kinds(&v) {
            Ok(kinds) => kinds,
            Err(reason) => {
                bad.get_or_insert_with(|| reason.to_owned());
                continue;
            }
        };
        let manifest = v
            .get("manifest_path")
            .and_then(aterm_json::Value::as_str)
            .and_then(|path| {
                manifests
                    .entry(path.to_owned())
                    .or_insert_with(|| {
                        let text = std::fs::read_to_string(path).ok()?;
                        aterm_toml::from_str::<aterm_toml::Value>(&text).ok()
                    })
                    .clone()
            });
        let harness = harness_of(manifest.as_ref(), &kinds, name);
        match target_flag(&kinds, name) {
            Some(flag) => exes.push((
                TestExe {
                    exe: PathBuf::from(exe),
                    flag,
                    bin_env: Vec::new(),
                    harness,
                },
                package.map(str::to_owned),
            )),
            None => {
                bad.get_or_insert_with(|| format!("a test executable of kind {kinds:?}: {exe}"));
            }
        }
    }
    *log = kept;
    if let Some(what) = bad {
        return Err(format!("cargo reported what this gate cannot read: {what}"));
    }
    Ok(exes
        .into_iter()
        .map(|(mut exe, package)| {
            exe.bin_env = bins
                .iter()
                .filter(|(of, _, _)| Some(of) == package.as_ref())
                .map(|(_, key, path)| (key.clone(), path.clone()))
                .collect();
            exe
        })
        .collect())
}

/// The bytes of a recording pass's log before cargo's first `Running` header: its compile's
/// prelude, where cargo's own messages are, and no test's output.
fn recording_prelude_len(log: &str) -> usize {
    log.split_inclusive('\n')
        .take_while(|line| {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.contains('\x1b') {
                crate::libtest::header(&crate::libtest::strip_ansi(line)).is_none()
            } else {
                crate::libtest::header(line).is_none()
            }
        })
        .map(str::len)
        .sum()
}

/// Read Cargo's artifact prelude without treating a test's own JSON as metadata.
fn take_recording_executables(log: &mut String) -> Result<Vec<TestExe>, String> {
    let tail = log.split_off(recording_prelude_len(log));
    let exes = take_test_executables(log);
    log.push_str(&tail);
    exes
}

/// One test binary the run will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    /// cargo's ` Running <target> (<path>)` line for it, verbatim.
    pub header: String,
    /// cargo's re-run spec.
    pub spec: String,
    pub record: Record,
    /// It runs libtest's harness ([`TestExe::harness`]).
    pub harness: bool,
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

/// Pair each record with the header cargo printed for it and the compile's flag for its
/// executable, and add its package's binaries to its environment ([`TestExe::bin_env`]).
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
        .map(|(mut record, (header, path))| {
            let exe = record.exe().to_path_buf();
            if !exe.ends_with(&path) {
                return Err(format!(
                    "cargo announced {path} where the recorder saw {}",
                    exe.display()
                ));
            }
            let found = exes
                .iter()
                .find(|e| e.exe == exe)
                .or_else(|| {
                    exes.iter().find(|e| {
                        e.exe.file_name().is_some() && e.exe.file_name() == exe.file_name()
                    })
                })
                .ok_or_else(|| format!("the compile did not report {}", exe.display()))?;
            let flag = &found.flag;
            record
                .env
                .retain(|(k, _)| !found.bin_env.iter().any(|(bin, _)| bin == k));
            record.env.extend(found.bin_env.iter().cloned());
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
                harness: found.harness,
            })
        })
        .collect()
}

/// Run every binary through [`pool`], `jobs` at a time: each one's run and
/// its own wall time. Call `completed` as each finishes, before waiting for
/// other binaries; its durations separate queue time from execution time.
pub fn run_all<F, E>(
    bins: &[Binary],
    jobs: usize,
    threads: u32,
    run_one: F,
    completed: E,
) -> Vec<(Run, Duration)>
where
    F: Fn(&Binary, u32) -> Run + Sync,
    E: Fn(&Binary, &Run, Duration, Duration) + Sync,
{
    let all: Vec<usize> = (0..bins.len()).collect();
    let queued = Instant::now();
    pool(bins.len(), &all, jobs, |i| {
        let started = Instant::now();
        let run = run_one(&bins[i], threads);
        let elapsed = started.elapsed();
        completed(&bins[i], &run, started.duration_since(queued), elapsed);
        (run, elapsed)
    })
    .into_iter()
    .map(|pooled| {
        pooled.unwrap_or_else(|| {
            let run = Run {
                stopped_at: None,
                ok: false,
                output: "aterm-verify: this test binary was never run (a gate defect)\n".into(),
                code: None,
                spawn_error: None,
                usage: None,
            };
            (run, Duration::ZERO)
        })
    })
    .collect()
}

/// The most bytes a `test-binary-finished` event spends on each failed case's output: with at
/// most [`crate::report::FINDINGS_SHOWN`] cases the line stays far under the 64 KiB the release's
/// reader holds a line to, so a FAIL is never lost to its own length.
pub const FAILURE_OUTPUT_BYTES: usize = 2048;

/// The test binaries a release's resume may carry (A19): each binary that passed before in this
/// tree, by its spec, with the SHA-256 its executable had then.
pub type Passed = BTreeMap<String, String>;

/// A `--passed-binaries` file: `<sha256> <spec>` on each line (the spec is the rest of the line),
/// blank lines ignored.
///
/// # Errors
/// A line with no spec, a digest that is not 64 lowercase hex digits, or one spec twice.
pub fn read_passed(text: &str) -> Result<Passed, String> {
    let mut passed = Passed::new();
    for (n, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let (sha, spec) = line
            .split_once(' ')
            .filter(|(sha, spec)| aterm_codec::hex::is_lower(sha, 64) && !spec.trim().is_empty())
            .ok_or_else(|| format!("line {} is not `<sha256> <spec>`: {line}", n + 1))?;
        if passed
            .insert(spec.trim().to_owned(), sha.to_owned())
            .is_some()
        {
            return Err(format!("line {} names {} twice", n + 1, spec.trim()));
        }
    }
    Ok(passed)
}

/// The SHA-256 of `exe`, lowercase hex, and what reading it took, added to `spent` (nanoseconds).
fn exe_sha256(exe: &Path, spent: &AtomicU64) -> Option<String> {
    use std::io::Read as _;
    let began = Instant::now();
    let mut file = std::fs::File::open(exe).ok()?;
    let mut hasher = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let sha = loop {
        match file.read(&mut buf) {
            Ok(0) => break Some(aterm_codec::hex::encode_lower(&hasher.finalize())),
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break None,
        }
    };
    let nanos = u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX);
    spent.fetch_add(nanos, Ordering::Relaxed);
    sha
}

/// What a release's resume does with its binaries ([`carry`]).
pub struct Carried {
    /// The binaries that run, in their order.
    pub run: Vec<Binary>,
    /// The binaries carried, each with its executable's SHA-256.
    pub carried: Vec<(Binary, String)>,
    /// The SHA-256 of each binary hashed to decide, by spec.
    pub known: BTreeMap<String, String>,
}

/// Which of `bins` a release's resume carries, and the SHA-256 of each binary it hashed (`hash`,
/// for each binary `passed` names): a binary is carried only when `passed` names its spec with
/// the digest its executable has now and its package is not [`crate::checks::GOLDEN`]'s, whose
/// test a release's run must show every time ([`decide`]). The rest run, in their order.
pub fn carry(
    passed: &Passed,
    bins: Vec<Binary>,
    hash: impl Fn(&Binary) -> Option<String> + Sync,
    jobs: usize,
) -> Carried {
    let golden = |b: &Binary| {
        b.record.var("CARGO_PKG_NAME").and_then(OsStr::to_str) == Some(crate::checks::GOLDEN.0)
    };
    let asked: Vec<usize> = (0..bins.len())
        .filter(|&i| passed.contains_key(&bins[i].spec) && !golden(&bins[i]))
        .collect();
    let hashes = pool(bins.len(), &asked, jobs, |i| hash(&bins[i]));
    let mut known = BTreeMap::new();
    let (mut run, mut carried) = (Vec::new(), Vec::new());
    for (b, sha) in bins.into_iter().zip(hashes) {
        let sha = sha.flatten();
        if let Some(sha) = &sha {
            known.insert(b.spec.clone(), sha.clone());
        }
        match sha {
            Some(sha) if passed.get(&b.spec) == Some(&sha) => carried.push((b, sha)),
            _ => run.push(b),
        }
    }
    Carried {
        run,
        carried,
        known,
    }
}

/// The `test-binary-finished` event of a binary a resume carried: it passed before in this tree
/// with these bytes, and runs no time now.
fn carried_event(binary: &Binary, sha256: &str) -> String {
    format!(
        "verify-event: {}",
        aterm_json::json!({
            "schema": 1, "event": "test-binary-finished", "test": binary.spec,
            "outcome": "ok", "exit_code": aterm_json::Value::Null,
            "waiting_s": 0.0, "execution_s": 0.0,
            "cpu_s": aterm_json::Value::Null, "starved_s": aterm_json::Value::Null,
            "peak_bytes": aterm_json::Value::Null,
            "width": aterm_json::Value::Null, "tests": aterm_json::Value::Null,
            "failures": [], "detail": "carried: it passed before in this tree with these bytes",
            "exe_sha256": sha256, "carried": true,
        })
    )
}

/// Write one structured line to stderr in one `write_all` of the line and its newline
/// (`eprintln!` writes the newline apart), and to a release's events file, where it lands whole
/// whatever else writes ([`Ctx::record_event`]): stderr is shared with every child, and a line
/// longer than a pipe writes at once can be split there.
fn emit(ctx: &crate::Ctx, event: &str) {
    let mut line = String::with_capacity(event.len() + 1);
    line.push_str(event);
    line.push('\n');
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
    ctx.record_event(&line);
}

/// A finished binary's outcome (ok, NOT RUN when its machine or the run's end kept it from
/// running, else FAIL, the proven red that ends a halted run), its `test-binary-finished` event,
/// and the sample of it its history keeps ([`crate::test_history`]) when its run measured what it
/// takes ([`measured`]; `started`: the run's ceiling left it time to start): its outcome, its
/// waits, its tree's usage (`null` where unread), the test threads it ran at (`width`) and the
/// tests libtest said it runs.
fn binary_event(
    binary: &Binary,
    run: &Run,
    started: bool,
    waiting: Duration,
    execution: Duration,
    width: u32,
    exe_sha256: Option<&str>,
) -> (Outcome, String, Option<crate::test_history::Sample>) {
    // The child has no cargo rerun footer. Classify it with the same known-binary
    // context as the final report, so passing fixture output cannot own a failure.
    let combined =
        (!run.ok).then(|| combined(std::slice::from_ref(binary), std::slice::from_ref(run)));
    let failures = combined
        .as_ref()
        .and_then(|all| crate::libtest::failed_tests(&all.output))
        .unwrap_or_default()
        .into_iter()
        .take(crate::report::FINDINGS_SHOWN)
        .map(|failed| {
            aterm_json::json!({
                "case": crate::report::clip_json(&failed.name, crate::report::EVENT_ROW_BYTES),
                "output": crate::report::clip_json(&failed.block, FAILURE_OUTPUT_BYTES),
                "refused": failed.refused,
            })
        })
        .collect::<Vec<_>>();
    let unavailable = if run.ok || run.spawn_error.is_some() {
        run.environment_failure()
    } else {
        combined.as_ref().and_then(Run::environment_failure)
    };
    let outcome = if run.ok {
        Outcome::Ok
    } else if run.stopped_at.is_some() {
        // Stopped at a decisive red case: a FAIL by construction (`libtest::decisive`).
        Outcome::Fail(Severity::GateFailed)
    } else if unavailable.is_some() {
        Outcome::Fail(Severity::CouldNotRun)
    } else {
        Outcome::Fail(Severity::GateFailed)
    };
    let tests = crate::libtest::tests_run(&run.output);
    let usage = run.usage;
    let event = format!(
        "verify-event: {}",
        aterm_json::json!({
            "schema": 1, "event": "test-binary-finished", "test": binary.spec,
            "outcome": crate::report::word(outcome), "exit_code": run.code,
            "waiting_s": waiting.as_secs_f64(), "execution_s": execution.as_secs_f64(),
            "cpu_s": usage.map(|u| u.cpu.as_secs_f64()),
            "starved_s": usage.map(|u| u.starved.as_secs_f64()),
            "peak_bytes": usage.map(|u| u.peak_bytes),
            "width": width, "tests": tests,
            "failures": failures,
            "detail": if run.ok {
                String::new()
            } else if let Some(stopped) = &run.stopped_at {
                stopped_detail(stopped, combined.as_ref().map_or("", |c| c.output.as_str()))
            } else {
                unavailable.unwrap_or_else(|| first_error(&run.output))
            },
            "stopped_at": run.stopped_at.as_ref().map(|s| s.case.as_str()),
            "exe_sha256": exe_sha256, "carried": false,
        })
    );
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let sample = measured(run, started).then(|| crate::test_history::Sample {
        at_ms: aterm_time::unix_millis().unwrap_or(0),
        outcome: crate::report::word(outcome).to_owned(),
        wall_ms: ms(execution),
        cpu_ms: usage.map(|u| ms(u.cpu)),
        starved_ms: usage.map(|u| ms(u.starved)),
        peak_bytes: usage.map(|u| u.peak_bytes),
        width,
        tests,
    });
    (outcome, event, sample)
}

/// A stopped binary's detail: its red case and the first line of what its block says panicked
/// (or the block's first line), as `<case>: <line>`.
fn stopped_detail(stopped: &crate::libtest_json::Stopped, log: &str) -> String {
    let block = crate::libtest::failed_tests(log)
        .unwrap_or_default()
        .into_iter()
        .find(|t| t.name == stopped.case)
        .map(|t| t.block)
        .unwrap_or_default();
    let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut line = lines.clone().next().unwrap_or("its block said nothing");
    while let Some(l) = lines.next() {
        if l.starts_with("thread '") && l.contains("panicked") {
            if let Some(next) = lines.next() {
                line = next;
            }
            break;
        }
    }
    format!(
        "{}: {}",
        stopped.case,
        line.chars().take(200).collect::<String>()
    )
}

/// Whether a binary's run measured what it takes, so its history keeps a sample of it: it was
/// `started` (the run's ceiling left it time: [`not_started`]), nothing outside it ended it (no
/// [`Run::spawn_error`]: a spawn refused, an interrupt that came before it started, a stuck
/// holder it waited behind, a wait for it that failed) and no interrupt was ending the run as it
/// ended ([`crate::exec::ending`]), which cuts a binary short at a point that says nothing of it.
/// A binary the run's ceiling ended keeps its sample, a lower bound of what it takes; one that
/// never ran would keep a wall of about 0 ms, and push a measured sample out of its row. A binary
/// verify stopped at its first red case ([`Run::stopped_at`]) keeps none either: its wall is where
/// verify ended it, and a sample with no wall is one the history's schema-1 readers (an older
/// checkout's verify, sharing the per-user file) cannot decode, so they would empty the file.
fn measured(run: &Run, started: bool) -> bool {
    started && run.stopped_at.is_none() && run.spawn_error.is_none() && !crate::exec::ending()
}

/// The run's long poles: its own wall, then its slowest binaries by their own
/// wall, longest first. The run cannot end before its longest binary, so a
/// binary near the run's wall is where a faster suite starts.
#[must_use]
pub fn poles(bins: &[Binary], walls: &[Duration], total: Duration) -> String {
    let mut order: Vec<usize> = (0..bins.len().min(walls.len())).collect();
    order.sort_by(|&a, &b| walls[b].cmp(&walls[a]));
    let named: Vec<String> = order
        .iter()
        .take(POLES_SHOWN)
        .map(|&i| {
            format!(
                "{} {}",
                bins[i].spec,
                crate::report::duration(walls[i].as_secs_f64())
            )
        })
        .collect();
    format!(
        "  test binaries' long poles: the run took {}; {}",
        crate::report::duration(total.as_secs_f64()),
        named.join(", ")
    )
}

/// Run `run(i)` for each distinct index below `n` in `order`, `jobs` at a time (at least one),
/// each worker booking its children into the calling stage's memory meter
/// ([`crate::memory::current`]) and handing them its cover marker ([`crate::memory::cover`]).
/// Workers also retain the actual stage reservation so a nested bounded walk can outlive
/// its pool callback without releasing memory still used by that walk.
pub fn pool<T, F>(n: usize, order: &[usize], jobs: usize, run: F) -> Vec<Option<T>>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    let answers: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let meter = crate::memory::current();
    let cover = crate::memory::cover();
    let memory = crate::current_memory_claim();
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, order.len().max(1)) {
            scope.spawn(|| {
                let _metering = crate::memory::metering(meter.clone());
                let _covering = crate::memory::covering(cover.clone());
                let _reservation = crate::holding_memory_claim(memory.clone());
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

/// The command that runs `b` as cargo would have, with `threads` test threads, the run's checker
/// policy and its `Running` header at the head of its log, its cases read as they end
/// ([`exec::CaseWatch`]): in libtest's JSON format, its flags right after the executable, where
/// `watch` says libtest took it and `b` may run with it ([`json_fits`]).
#[must_use]
pub fn binary_cmd(b: &Binary, threads: u32, checkers: bool, watch: Watch) -> Cmd {
    let mut env: Vec<(OsString, OsString)> = b
        .record
        .env
        .iter()
        .filter(|(k, _)| k != "RUST_TEST_THREADS" && k != crate::stages::CHECKERS_ENV)
        .cloned()
        .collect();
    env.push(("RUST_TEST_THREADS".into(), threads.to_string().into()));
    let mut command = Cmd::replayed(&b.record.argv, &b.record.cwd, env)
        .preamble(format!("{}\n", b.header))
        .counting_waits();
    let json = watch.json && json_fits(b);
    if json {
        let at = 0..0;
        command
            .args
            .splice(at, JSON_FLAGS.iter().map(OsString::from));
    }
    command.test_cases = Some(exec::CaseWatch {
        spec: b.spec.clone(),
        json,
        stop_at_red: watch.stop_at_red,
    });
    crate::stages::with_checkers(command, checkers)
}

/// The one log cargo would have written for these runs, in declared order, except that a run with
/// a `spawn_error` (something outside the binary ended it: [`split_unrun`]) adds its own output
/// alone: it is no failed target, and the returned `ok` says nothing of it. Its own row says why
/// ([`decide_runs`]).
#[must_use]
pub fn combined(bins: &[Binary], runs: &[Run]) -> Run {
    let mut output = String::new();
    let mut failed = Vec::new();
    for (b, r) in bins.iter().zip(runs) {
        output.push_str(&r.output);
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if r.ok || r.spawn_error.is_some() {
            continue;
        }
        output.push_str(&format!("error: test failed, to rerun pass `{}`\n", b.spec));
        if let Some(stopped) = &r.stopped_at {
            // Verify ended it, by design: no abnormal exit to say.
            output.push_str(&format!(
                "note: verify ended this binary at its first red case `{}`\n",
                stopped.case
            ));
        } else if r.code != Some(101) {
            let argv: Vec<String> = b
                .record
                .argv
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let why = r.code.map_or_else(
                || "killed by a signal".to_string(),
                |c| format!("exit status: {c}"),
            );
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
        stopped_at: None,
        ok: failed.is_empty(),
        output,
        code: Some(if failed.is_empty() { 0 } else { 101 }),
        spawn_error: None,
        usage: None,
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
pub fn not_started(b: &Binary, limit: Duration) -> Run {
    Run {
        stopped_at: None,
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
        usage: None,
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

/// Decide the test run's row exactly as the serial child's was decided. A test
/// whose macOS service (`trustd`, the disk-image service) did not answer ends
/// COULD NOT RUN itself (`aterm_uds::deadline::require_trust_service`), so its
/// row says so through [`crate::libtest::environment_refusals`]. Under `release`, and under the
/// `--as-release` its red row prints, a passing run that did not pass
/// [`crate::checks::GOLDEN`]'s test is COULD NOT RUN too, whichever way its binaries were run.
fn decide(ctx: &crate::Ctx, r: &mut Report, run: &Run, label: &str, env: &[(OsString, OsString)]) {
    if ctx.checkers_in_tests && crate::stages::compared_no_frame(run) {
        r.cannot_run(format!(
            "{label} — {} did not run, so no frame was held to its twin",
            crate::checks::GOLDEN.1
        ));
        return;
    }
    r.decide_test_child(run, label, env);
}

/// The binaries whose runs carry a `spawn_error`, each with it, apart from the rest with their
/// runs: something outside the binary ended it (a spawn refused, an interrupt, a stuck holder it
/// waited behind, a wait for it that failed), so its run decided nothing about the tree.
fn split_unrun<'a>(
    bins: &'a [Binary],
    runs: &'a [Run],
) -> (Vec<(&'a Binary, &'a str)>, Vec<Binary>, Vec<Run>) {
    let mut unrun = Vec::new();
    let (mut ran_bins, mut ran) = (Vec::new(), Vec::new());
    for (b, run) in bins.iter().zip(runs) {
        match &run.spawn_error {
            Some(why) => unrun.push((b, why.as_str())),
            None => {
                ran_bins.push(b.clone());
                ran.push(run.clone());
            }
        }
    }
    (unrun, ran_bins, ran)
}

/// Decide the test run's rows from each binary's run: one COULD NOT RUN row per binary
/// [`split_unrun`] takes out, `label — spec: why`, in declared order, then `decide` over the
/// [`combined`] log of the rest, unless it took out every binary (so a run of unrun binaries
/// alone has no [`crate::libtest::RAN_NO_TEST`] row). No binary at all, a resume that carried
/// every one ([`carry`]), is decided as before.
fn decide_runs(
    r: &mut Report,
    bins: &[Binary],
    runs: &[Run],
    label: &str,
    decide: impl FnOnce(&mut Report, &Run),
) {
    let (unrun, bins, runs) = split_unrun(bins, runs);
    let none_ran = runs.is_empty() && !unrun.is_empty();
    for (b, why) in unrun {
        r.cannot_run(format!("{label} — {}: {why}", b.spec));
    }
    if !none_ran {
        decide(r, &combined(&bins, &runs));
    }
}

/// Record Cargo's current test invocations and run them with the admitted parallelism
/// ([`run_planned`]). Each binary that runs libtest's harness runs in libtest's JSON format where
/// libtest takes it ([`probe_json`]), so each failed case's block is read as the case ends. The
/// first proven red ends the run when its halt is armed ([`crate::Ctx::halt`], named for
/// `check`): a failed case whose block, with those before it, makes its binary a FAIL
/// (`libtest::decisive`) ends that binary there, its log rendered as libtest's report of the cases
/// that finished ([`crate::libtest_json::render`]); a case that refused its machine or hit a full
/// disk ends nothing. A binary run in the human format (no harness, a refused probe, its own
/// output flags) is decided at its end. No binary starts after the red, and those it stopped are
/// one NOT RUN row with the halt's words. A binary verify could not start or ended unrun
/// otherwise is a NOT RUN row of its own, and the binaries that ran decide the run's row
/// ([`decide_runs`]).
pub fn run_tests(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    serial: &Cmd,
    check: crate::checks::Check,
) {
    let mut cores = None;
    let mut host_cores = || *cores.get_or_insert_with(|| ctx.cores());
    let managed_share = match crate::test_workers::admitted_share(&ctx.env) {
        Ok(share) => share,
        Err(why) => {
            r.cannot_run(format!("{why}; no test was run"));
            return;
        }
    };
    let asked_cores = managed_share.unwrap_or_else(&mut host_cores).max(1);
    let _subprocess_claim = if ctx.memory.is_some() {
        match crate::admit_live(
            aterm_capacity::Need::new(format!("verify {label} subprocesses")).cores(asked_cores),
            aterm_capacity::Admit::Wait,
        ) {
            Ok(claim) => Some(claim),
            Err(aterm_capacity::Error::Stopped { .. }) if ctx.halted().is_some() => {
                r.cannot_run(format!(
                    "{label} — {}; no test was run",
                    ctx.halted().unwrap_or_default()
                ));
                return;
            }
            Err(why) => {
                r.cannot_run(format!(
                    "subprocess CPU admission refused: {why}; no test was run"
                ));
                return;
            }
        }
    } else {
        None
    };
    let granted_cores = match _subprocess_claim.as_ref().map(aterm_capacity::Claim::cores) {
        Some(0) => {
            r.cannot_run("subprocess CPU admission granted no cores; no test was run");
            return;
        }
        Some(cores) => cores.min(asked_cores),
        None => asked_cores,
    };
    let (jobs, threads) = ctx.test_workers_for(granted_cores, host_cores);
    let thread_override = (ctx.env.build_slot_held.is_some()
        || ctx.test_threads.is_some()
        || granted_cores < asked_cores)
        .then_some(threads);
    let slots = match subprocess_pool(&ctx.scratch, subprocess_slots_for_cores(granted_cores)) {
        Ok(pool) => pool,
        Err(why) => {
            r.cannot_run(format!(
                "cannot initialize shared subprocess pool: {why}; no test was run"
            ));
            return;
        }
    };

    let mut serial = thread_override.map_or_else(
        || serial.clone(),
        |threads| serial.clone().env("RUST_TEST_THREADS", threads.to_string()),
    );
    let checkers = check == crate::checks::Check::Checkers || ctx.checkers_in_tests;
    serial = crate::stages::with_checkers(serial, checkers).env(SUBPROCESS_SLOTS_ENV, slots.path());
    let serial = &serial;
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
    let env = crate::findings::rerun_env(serial, thread_override);
    let serially = |r: &mut Report, why: &str| {
        r.raw(format!(
            "  test binaries: run by cargo, one at a time — {why}"
        ));
        let out = exec::run(serial, ctx.exec_env());
        r.raw(out.output.as_str());
        decide(ctx, r, &out, label, &env);
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
    let began = Instant::now();
    let mut pass = exec::run(&recording_cmd(serial, &config), ctx.exec_env());
    let pass_wall = began.elapsed();
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
        decide(ctx, r, &pass, label, &env);
        return;
    }
    let account = crate::compile_account::Account::of(
        crate::stages::step_of(r),
        &format!("{label} (recording pass)"),
        Some(&pass.output[..recording_prelude_len(&pass.output)]),
        pass.usage,
        pass_wall,
    );
    r.raw(format!("  {}", account.line()));
    eprintln!("{}", account.event());
    let exes = take_recording_executables(&mut pass.output);
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
    let preface: String = pass
        .output
        .lines()
        .filter(|l| !l.trim_start().starts_with("Running "))
        .map(|l| format!("{l}\n"))
        .collect();
    let planned = Planned {
        bins,
        jobs,
        threads,
        cores: usize::try_from(granted_cores).unwrap_or(1),
        checkers,
        preface,
    };
    run_planned(ctx, r, label, planned, check, &env);
}

/// What [`run_tests`] planned for its binaries' runs.
pub(crate) struct Planned {
    /// The binaries, in declared order.
    pub(crate) bins: Vec<Binary>,
    /// How many run at once.
    pub(crate) jobs: usize,
    /// The test threads each runs at.
    pub(crate) threads: u32,
    /// The cores the run was granted (a resume's hashing width, [`carry`]).
    pub(crate) cores: usize,
    /// The run's checker policy ([`crate::stages::with_checkers`]).
    pub(crate) checkers: bool,
    /// The recording pass's log, its `Running` headers dropped, at the head of a whole run's
    /// report.
    pub(crate) preface: String,
}

/// What [`probe_json`] learned of libtest's JSON format.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probe {
    /// A binary listed its cases in it.
    Took,
    /// A binary ran and exited without it: what libtest said, its first line.
    Refused(String),
    /// No binary it asked answered (a spawn refused, a crash, a run already ending): why the
    /// last one did not.
    Unanswered(String),
}

/// What a binary's run reads of libtest's JSON format, by [`probe_json`]: whether its binaries
/// may run with it, and why not.
fn json_ladder(probe: &Probe) -> String {
    match probe {
        Probe::Took => "  test cases: seen as they fail (libtest JSON)".to_owned(),
        Probe::Refused(why) => {
            format!("  test cases: decided at each binary's end — libtest refused JSON: {why}")
        }
        Probe::Unanswered(why) => format!(
            "  test cases: decided at each binary's end — no binary answered libtest's JSON \
             probe: {why}"
        ),
    }
}

/// libtest's JSON flags, which go right after the executable.
const JSON_FLAGS: [&str; 4] = ["-Z", "unstable-options", "--format", "json"];

/// The binaries [`probe_json`] asks before it takes no answer for one.
const PROBE_TRIES: usize = 3;

/// Ask the binaries that run libtest's harness ([`Binary::harness`]), in order, to list their
/// cases in libtest's JSON format until one answers, at most [`PROBE_TRIES`] of them: `None`
/// where no binary runs the harness. A binary that exited with a code answers: [`Probe::Took`]
/// where it exited 0 and printed a suite `discovery` event, else [`Probe::Refused`] with its first
/// line, what libtest said in refusing. One that verify could not start or ended
/// ([`Run::spawn_error`]), that a signal ended, or that ended as the run was ending says nothing
/// of libtest, so the next is asked; with none left, [`Probe::Unanswered`].
fn probe_json(ctx: &crate::Ctx, bins: &[Binary]) -> Option<Probe> {
    let first_line = |run: &Run| -> String {
        run.output
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output")
            .trim()
            .chars()
            .take(200)
            .collect()
    };
    let mut unanswered = None;
    for b in bins.iter().filter(|b| b.harness).take(PROBE_TRIES) {
        let mut argv = vec![b.record.exe().as_os_str().to_owned()];
        argv.extend(JSON_FLAGS.iter().map(OsString::from));
        argv.push("--list".into());
        let mut cmd = Cmd::replayed(&argv, &b.record.cwd, b.record.env.clone());
        cmd.capture = exec::Capture::Emit;
        let run = exec::run(&cmd, ctx.exec_env());
        if let Some(why) = &run.spawn_error {
            unanswered = Some(format!("{}: {why}", b.spec));
            continue;
        }
        if run.code.is_none() || crate::exec::ending() {
            unanswered = Some(format!("{}: {}", b.spec, first_line(&run)));
            continue;
        }
        let discovered = run.output.lines().any(|l| {
            crate::libtest_json::parse_event(l).is_some_and(|ev| {
                ev.kind == crate::libtest_json::Kind::Suite && ev.event == "discovery"
            })
        });
        return Some(if run.ok && discovered {
            Probe::Took
        } else {
            Probe::Refused(first_line(&run))
        });
    }
    unanswered.map(Probe::Unanswered)
}

/// Whether `b` may run with libtest's JSON flags: it runs libtest's harness, nothing its record
/// hands it already chooses an output (a format, quiet, a list, uncaptured output, a log file, a
/// benchmark run), which the flags would contradict or a reader of the events would miss, and
/// nothing would change what it runs once `-Z unstable-options` lets libtest read its unstable
/// inputs: no `-Z` of its own (which every unstable flag, `--shuffle`, `--ensure-time`, …, needs)
/// and no `RUST_TEST_SHUFFLE`/`RUST_TEST_SHUFFLE_SEED`, which libtest reads only then and which
/// would reorder its cases and add a `shuffle_seed` to its events.
fn json_fits(b: &Binary) -> bool {
    const CHOSEN: [&str; 9] = [
        "--format",
        "-q",
        "--quiet",
        "--list",
        "--nocapture",
        "--no-capture",
        "--show-output",
        "--logfile",
        "--bench",
    ];
    b.harness
        && !b.record.argv.iter().skip(1).any(|a| {
            let a = a.to_string_lossy();
            CHOSEN
                .iter()
                .any(|c| *a == **c || a.strip_prefix(c).is_some_and(|r| r.starts_with('=')))
        })
        && !b
            .record
            .argv
            .iter()
            .skip(1)
            .any(|a| a.to_string_lossy().starts_with("-Z"))
        && [
            "RUST_TEST_NOCAPTURE",
            "RUST_TEST_SHUFFLE",
            "RUST_TEST_SHUFFLE_SEED",
        ]
        .iter()
        .all(|k| b.record.var(k).is_none())
}

/// How the run reads its binaries' cases ([`binary_cmd`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    /// libtest took its JSON format ([`probe_json`]).
    pub json: bool,
    /// The run halts at its first red ([`crate::sched::Halt::armed`]).
    pub stop_at_red: bool,
}

/// Run the planned binaries: each through [`pool`], its cases read as they end ([`Watch`]), and
/// decide the run's rows ([`decide_runs`]). The first binary that fails ends the run when its halt
/// is armed: a JSON run ends that binary at its first decisive red case (`exec`'s `LiveCases`), a
/// human one at its end; no binary starts after it, and those it stopped are one NOT RUN row with
/// the halt's words.
pub(crate) fn run_planned(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    planned: Planned,
    check: crate::checks::Check,
    env: &[(OsString, OsString)],
) {
    let Planned {
        bins,
        jobs,
        threads,
        cores,
        checkers,
        preface,
    } = planned;
    // A release hashes each binary's executable (its record's key for a resume, A19); a dev run
    // keeps none, so it pays nothing for it.
    let hashed = AtomicU64::new(0);
    let hash = |b: &Binary| {
        ctx.release
            .then(|| exe_sha256(b.record.exe(), &hashed))
            .flatten()
    };
    let listed = bins.len();
    let Carried {
        run: bins,
        carried,
        known,
    } = match &ctx.passed_binaries {
        Some(passed) => carry(passed, bins, hash, cores),
        None => Carried {
            run: bins,
            carried: Vec::new(),
            known: BTreeMap::new(),
        },
    };
    for (binary, sha) in &carried {
        emit(ctx, &carried_event(binary, sha));
    }
    let probe = probe_json(ctx, &bins);
    if let Some(probe) = &probe {
        r.raw(json_ladder(probe));
    }
    let watch = Watch {
        json: probe == Some(Probe::Took),
        stop_at_red: ctx.halt.armed(),
    };
    let shas = Mutex::new(known);
    let ceiling = ctx.exec_env().child_ceiling;
    let started = Instant::now();
    let samples = Mutex::new(Vec::with_capacity(bins.len()));
    // The binaries the ceiling left no time to start, by spec: their samples are not kept.
    let unstarted = Mutex::new(std::collections::BTreeSet::new());
    let timed = run_all(
        &bins,
        jobs,
        threads,
        |b, t| {
            if let Some(words) = ctx.halted() {
                return Run {
                    ok: false,
                    output: String::new(),
                    code: None,
                    spawn_error: Some(words.to_owned()),
                    ..Run::default()
                };
            }
            let known = shas
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&b.spec);
            if !known && let Some(sha) = hash(b) {
                shas.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(b.spec.clone(), sha);
            }
            let mut env = ctx.exec_env();
            if let Some(limit) = ceiling {
                let left = limit.saturating_sub(started.elapsed());
                if left.is_zero() {
                    unstarted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(b.spec.clone());
                    return not_started(b, limit);
                }
                env.child_ceiling = Some(left);
            }
            let cmd = binary_cmd(b, t, checkers, watch);
            let mut run = exec::run(&cmd, env);
            if cmd.test_cases.as_ref().is_some_and(|w| w.json) {
                // The raw events were the child's log; every reader takes libtest's report.
                run.output = crate::libtest_json::render(&run.output, run.stopped_at.as_ref());
            }
            run
        },
        |binary, run, waiting, execution| {
            let never_started = unstarted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&binary.spec);
            let sha = shas
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&binary.spec)
                .cloned();
            let (outcome, event, sample) = binary_event(
                binary,
                run,
                !never_started,
                waiting,
                execution,
                threads,
                sha.as_deref(),
            );
            emit(ctx, &event);
            if outcome == Outcome::Fail(Severity::GateFailed) {
                ctx.halt.red(check.name());
            }
            if let Some(sample) = sample {
                samples
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((binary.spec.clone(), sample));
            }
        },
    );
    let total = started.elapsed();
    let samples = samples
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let kept = ctx
        .test_history
        .as_deref()
        .map(|path| crate::test_history::keep(path, samples));
    let (runs, walls): (Vec<Run>, Vec<Duration>) = timed.into_iter().unzip();
    // What a release's resume carried and what hashing the executables cost, in a halted run's
    // report as in a whole one's.
    let accounted = |r: &mut Report| {
        if ctx.passed_binaries.is_some() {
            r.raw(format!(
                "  test binaries: {} of {listed} carried (each passed before in this tree, its \
                 executable unchanged); {} run",
                carried.len(),
                bins.len()
            ));
        }
        if ctx.release {
            r.raw(format!(
                "  test binaries hashed (SHA-256 of each executable, a resume's key): {} in {} \
                 summed over workers",
                shas.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
                crate::report::duration(
                    Duration::from_nanos(hashed.load(Ordering::Relaxed)).as_secs_f64()
                )
            ));
        }
    };
    if let Some(words) = ctx.halted()
        && runs
            .iter()
            .any(|run| run.spawn_error.as_deref() == Some(words))
    {
        let (stopped, done): (Vec<_>, Vec<_>) = bins
            .iter()
            .zip(&runs)
            .partition(|(_, run)| run.spawn_error.as_deref() == Some(words));
        r.raw(summary(&bins, jobs, threads));
        accounted(r);
        r.raw(poles(&bins, &walls, total));
        if let Some(Err(why)) = &kept {
            r.raw(format!(
                "  test binaries' history not kept (planning data only): {why}"
            ));
        }
        if !done.is_empty() {
            let (bins, runs): (Vec<Binary>, Vec<Run>) = done
                .into_iter()
                .map(|(b, run)| (b.clone(), run.clone()))
                .unzip();
            r.raw(combined(&bins, &runs).output);
            // The golden test may be among those stopped, and the run is red already.
            decide_runs(r, &bins, &runs, label, |r, rest| {
                r.decide_test_child(rest, label, env);
            });
        }
        r.cannot_run(format!(
            "{label} — {} of {} test binaries not run: {words}",
            stopped.len(),
            bins.len()
        ));
        return;
    }
    r.raw(preface);
    r.raw(summary(&bins, jobs, threads));
    accounted(r);
    r.raw(combined(&bins, &runs).output);
    r.raw(poles(&bins, &walls, total));
    if let Some(Err(why)) = kept {
        r.raw(format!(
            "  test binaries' history not kept (planning data only): {why}"
        ));
    }
    decide_runs(r, &bins, &runs, label, |r, rest| {
        decide(ctx, r, rest, label, env);
    });
}

static RECORDING: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
#[path = "testrun_environment_tests.rs"]
mod environment_tests;

#[cfg(test)]
#[path = "testrun_discovery_tests.rs"]
mod discovery_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_subprocess_pool_initialization_refuses_before_test_dispatch() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let scratch = tmp.path().join("not-a-directory");
        std::fs::write(&scratch, b"retained").expect("block pool initialization");
        let ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            scratch.clone(),
        );
        let mut report = Report::new("pool refusal");
        run_tests(
            &ctx,
            &mut report,
            "fixture",
            &Cmd::new("must-not-be-dispatched"),
            crate::checks::Check::Test,
        );
        assert!(
            matches!(&report.entries[..], [crate::ladder::Entry::Ladder {
            outcome: crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun), label, ..
        }] if label.starts_with("cannot initialize shared subprocess pool:"))
        );
        assert_eq!(
            std::fs::read(&scratch).expect("retained blocker"),
            b"retained"
        );
    }

    /// A release holds its run to the golden test on the path cargo runs the binaries on, when the
    /// recording left nothing: the serial fallback. CONTROL: the name that test really has passes,
    /// and a run that is no release asks nothing of it.
    #[cfg(unix)]
    #[test]
    fn a_release_refuses_a_serial_run_that_never_reached_the_golden_test() {
        use std::os::unix::fs::PermissionsExt as _;
        let row_of = |test: &str, as_run: fn(crate::Ctx) -> crate::Ctx| {
            let tmp = aterm_tempfile::tempdir().expect("scratch");
            let targo = tmp.path().join("targo");
            std::fs::write(
                &targo,
                format!(
                    "#!/bin/sh\n\
                     if [ \"$1\" = --config ]; then\n\
                     printf '     Running unittests src/lib.rs (target/debug/deps/aterm_gpu-1)\\n'\n\
                     exit 0\n\
                     fi\n\
                     printf 'running 1 test\\ntest {test} ... ok\\n\\n'\n\
                     printf 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 \
                     filtered out; finished in 0.00s\\n'\n"
                ),
            )
            .expect("the stand-in");
            std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
            let ctx = as_run(crate::Ctx::new(
                tmp.path().to_path_buf(),
                crate::Scope::workspace(),
                crate::EnvSnapshot::default(),
                tmp.path().to_path_buf(),
            ));
            let mut report = Report::new("the test run");
            run_tests(
                &ctx,
                &mut report,
                "fixture",
                &Cmd::new(&targo),
                crate::checks::Check::Test,
            );
            let raw: Vec<_> = report.entries.iter().map(|e| format!("{e:?}")).collect();
            assert!(
                raw.iter()
                    .any(|e| e.contains("run by cargo, one at a time")),
                "the fixture took the serial path: {raw:?}"
            );
            let mut rows = report.entries.iter().filter_map(|entry| match entry {
                crate::ladder::Entry::Ladder { outcome, label, .. } => {
                    Some((*outcome, label.clone()))
                }
                _ => None,
            });
            let row = rows.next().expect("a row");
            assert!(rows.next().is_none(), "{raw:?}");
            row
        };
        let row = |test: &str, release: bool| {
            row_of(
                test,
                if release {
                    |c| c.with_release(true)
                } else {
                    |c| c
                },
            )
        };
        let golden = crate::checks::GOLDEN.1;
        let renamed = "frame_record_tests::renamed";
        let (refused, label) = row(renamed, true);
        assert_eq!(
            refused,
            crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun),
            "{label}"
        );
        assert!(
            label.contains(golden) && label.contains("did not run"),
            "{label}"
        );
        assert_eq!(
            row(&format!("renderer::{golden}"), true).0,
            crate::ladder::Outcome::Ok
        );
        assert_eq!(row(renamed, false).0, crate::ladder::Outcome::Ok);
        assert_eq!(
            row_of(renamed, |c| c.with_checkers_in_tests(true)).0,
            crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun),
            "the command a release's red row prints refuses what the release did"
        );
    }

    #[test]
    fn subprocess_pool_cleanup_owns_only_its_new_directory() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let existing = tmp.path().join("existing");
        std::fs::create_dir(&existing).expect("owned existing fixture");
        init_subprocess_pool_dir(&existing, 2).expect("existing pool");
        let sentinel = existing.join("sentinel");
        std::fs::write(&sentinel, b"retained").expect("sentinel");
        assert!(init_subprocess_pool_dir(&existing, 2).is_err());
        let owned = subprocess_pool(tmp.path(), 2).expect("owned pool");
        let owned_path = owned.path().to_path_buf();
        assert_ne!(owned_path, existing);
        drop(owned);
        assert!(!owned_path.exists());
        assert_eq!(
            std::fs::read(&sentinel).expect("existing sentinel"),
            b"retained"
        );
        assert!(existing.join("slot-0000.lock").is_file());
        assert!(existing.join("slot-0001.lock").is_file());
    }

    #[test]
    fn failed_binary_is_reported_while_another_binary_is_still_running() {
        let bins: Vec<_> = ["slow", "failed"]
            .into_iter()
            .map(|name| Binary {
                harness: false,
                header: name.into(),
                spec: name.into(),
                record: rec("/r/test", "test"),
            })
            .collect();
        let released = (Mutex::new(false), std::sync::Condvar::new());
        let reported = Mutex::new(Vec::new());
        let runs = run_all(
            &bins,
            2,
            1,
            |binary, _| {
                if binary.spec == "slow" {
                    let (guard, timeout) = released
                        .1
                        .wait_timeout_while(
                            released.0.lock().unwrap(),
                            Duration::from_secs(60),
                            |ready| !*ready,
                        )
                        .unwrap();
                    assert!(
                        *guard && !timeout.timed_out(),
                        "completed failure was buffered"
                    );
                }
                Run {
                    stopped_at: None,
                    ok: binary.spec == "slow",
                    output: String::new(),
                    code: Some(i32::from(binary.spec != "slow")),
                    spawn_error: None,
                    usage: None,
                }
            },
            |binary, _, _, _| {
                reported.lock().unwrap().push(binary.spec.clone());
                if binary.spec == "failed" {
                    *released.0.lock().unwrap() = true;
                    released.1.notify_all();
                }
            },
        );
        assert_eq!(*reported.lock().unwrap(), ["failed", "slow"]);
        assert!(runs[0].0.ok);
        assert!(!runs[1].0.ok);
    }

    #[test]
    fn a_passing_binary_cannot_forge_a_failure_event_in_its_output() {
        let binary = Binary {
            harness: false,
            header: "test".into(),
            spec: "-p test".into(),
            record: rec("/r/test", "test"),
        };
        let mut run = Run {
            stopped_at: None,
            ok: true,
            output: "test counterfeit ... FAILED".into(),
            code: Some(0),
            spawn_error: None,
            usage: None,
        };
        let parse = |run: &Run| -> aterm_json::Value {
            let (_, line, _) =
                binary_event(&binary, run, true, Duration::ZERO, Duration::ZERO, 1, None);
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
        };
        assert_eq!(parse(&run)["outcome"].as_str(), Some("ok"));
        assert_eq!(parse(&run)["failures"].as_array().unwrap().len(), 0);
        run.ok = false;
        run.spawn_error = Some("test executable unavailable".into());
        assert_eq!(parse(&run)["outcome"].as_str(), Some("NOT RUN"));
    }

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

    /// A resume carries a binary only when the spec that passed before names the digest its
    /// executable has now, and never GOLDEN's package, whose test a release must show every run;
    /// the rest run in their order, and each binary hashed is known by its digest. Negative
    /// control: dropping the GOLDEN filter carries `aterm-gpu --lib`.
    #[test]
    fn a_resume_carries_only_unchanged_binaries_that_passed_and_never_the_golden_package() {
        let bin = |exe: &str, pkg: &str| Binary {
            harness: false,
            header: format!("     Running unittests src/lib.rs ({exe})"),
            spec: format!("-p {pkg} --lib"),
            record: rec(exe, pkg),
        };
        let bins = vec![
            bin("/t/deps/aterm_db-1", "aterm-db"),
            bin("/t/deps/aterm_grid-2", "aterm-grid"),
            bin("/t/deps/aterm_gpu-3", crate::checks::GOLDEN.0),
            bin("/t/deps/aterm_core-4", "aterm-core"),
        ];
        let digest = |exe: &Path| match exe.to_str() {
            Some("/t/deps/aterm_db-1") => "aa".repeat(32),
            Some("/t/deps/aterm_grid-2") => "bb".repeat(32),
            Some("/t/deps/aterm_gpu-3") => "cc".repeat(32),
            _ => "dd".repeat(32),
        };
        let passed = read_passed(&format!(
            "{} -p aterm-db --lib\n\n{} -p aterm-grid --lib\n{} -p {} --lib\n",
            "aa".repeat(32),
            "ee".repeat(32),
            "cc".repeat(32),
            crate::checks::GOLDEN.0
        ))
        .unwrap();
        let hashed = Mutex::new(Vec::new());
        let Carried {
            run,
            carried,
            known,
        } = carry(
            &passed,
            bins,
            |b| {
                hashed.lock().unwrap().push(b.spec.clone());
                Some(digest(b.record.exe()))
            },
            2,
        );
        let specs = |v: &[Binary]| v.iter().map(|b| b.spec.clone()).collect::<Vec<_>>();
        assert_eq!(
            carried
                .iter()
                .map(|(b, sha)| (b.spec.as_str(), sha.as_str()))
                .collect::<Vec<_>>(),
            [("-p aterm-db --lib", "aa".repeat(32).as_str())],
            "only the unchanged binary that passed"
        );
        assert_eq!(
            specs(&run),
            [
                "-p aterm-grid --lib",
                "-p aterm-gpu --lib",
                "-p aterm-core --lib"
            ],
            "a changed executable runs, GOLDEN's package always runs, an unnamed one runs"
        );
        let mut hashed = hashed.into_inner().unwrap();
        hashed.sort();
        assert_eq!(
            hashed,
            ["-p aterm-db --lib", "-p aterm-grid --lib"],
            "only what passed is hashed to decide"
        );
        assert_eq!(known.get("-p aterm-grid --lib"), Some(&"bb".repeat(32)));
        let event: aterm_json::Value = aterm_json::from_str(
            carried_event(&carried[0].0, &carried[0].1)
                .strip_prefix("verify-event: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(event["outcome"].as_str(), Some("ok"));
        assert_eq!(event["carried"].as_bool(), Some(true));
        assert_eq!(event["execution_s"].as_f64(), Some(0.0));
        assert_eq!(event["exe_sha256"].as_str(), Some("aa".repeat(32).as_str()));

        for bad in [
            "aa -p x --lib".to_owned(),
            format!("{} ", "aa".repeat(32)),
            format!("{} -p x --lib", "AA".repeat(32)),
            format!("{0} -p x --lib\n{0} -p x --lib", "aa".repeat(32)),
        ] {
            assert!(read_passed(&bad).is_err(), "{bad:?}");
        }
    }

    /// A test binary's event lands in a release's events file whole, as its cutter reads it, beside
    /// stderr. Negative control: an `emit` that writes stderr alone leaves the file empty.
    #[test]
    fn a_binarys_event_reaches_the_release_events_file_whole() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let at = dir.path().join("06-verify.events");
        let ctx = crate::Ctx::new(
            dir.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            dir.path().to_path_buf(),
        )
        .with_events(Some(crate::event_log::EventLog::open(&at).unwrap()));
        let binary = Binary {
            harness: false,
            header: String::new(),
            spec: "-p aterm-db --lib".into(),
            record: Record {
                cwd: dir.path().to_path_buf(),
                argv: vec!["/t/deps/aterm_db-1".into()],
                env: Vec::new(),
            },
        };
        let event = carried_event(&binary, &"aa".repeat(32));
        emit(&ctx, &event);
        assert_eq!(std::fs::read_to_string(&at).unwrap(), format!("{event}\n"));
    }

    /// A RECORD IS THE PROCESS, BYTE FOR BYTE: every argument and variable
    /// survives, spaces, `=` and newlines included, and a byte stream cut
    /// mid-token or missing its header is refused rather than read short.
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
        let inherited = Record {
            env: vec![
                ("K".into(), "first".into()),
                ("Path".into(), "a".into()),
                ("K".into(), "last".into()),
            ],
            ..r.clone()
        };
        assert_eq!(inherited.var("Path"), Some(OsStr::new("a")));
        assert_eq!(
            inherited.var("K"),
            Some(OsStr::new("last")),
            "the last setting wins"
        );
        assert_eq!(
            inherited.var("PATH"),
            cfg!(windows).then_some(OsStr::new("a")),
            "a name in another case is the same variable on Windows alone"
        );
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
                "--message-format=json-render-diagnostics",
                "--",
                "--skip",
                "x"
            ],
            "the runner before the subcommand, beside the build cache's --config (one after \
             it would replace the cache's); libtest's arguments untouched"
        );
    }

    #[test]
    fn recording_keeps_the_runner_with_command_local_workspace_features() {
        let serial = Cmd::new("targo").args([
            "--unverified",
            "test",
            "--config",
            "resolver.feature-unification=\"workspace\"",
            "--tests",
            "--",
            "--skip",
            "x",
        ]);
        let command = recording_cmd(&serial, "RUNNER");
        let args = command.argv();
        assert_eq!(
            &args[1..7],
            [
                "--unverified",
                "test",
                "--config",
                "RUNNER",
                "--config",
                "resolver.feature-unification=\"workspace\""
            ]
        );
        assert!(args.ends_with(&["--".to_owned(), "--skip".to_owned(), "x".to_owned()]));
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

    #[test]
    fn mixed_type_target_kinds_do_not_select_test_executables() {
        let valid = artifact("/built/x", "lib", "x", true);
        for kinds in [r#"["lib"]"#, r#"["rlib","lib"]"#] {
            let mut positive = valid.replace(r#"["lib"]"#, kinds);
            assert_eq!(take_test_executables(&mut positive).unwrap().len(), 1);
        }
        for kinds in [r#"["lib",7]"#, r#"[null,"lib"]"#, r#"["lib",{}]"#] {
            let malformed = valid.replace(r#"["lib"]"#, kinds);
            for mut wire in [
                malformed.clone(),
                format!("{valid}{malformed}"),
                format!("{malformed}{valid}"),
            ] {
                assert!(take_test_executables(&mut wire).is_err(), "{kinds}");
            }
        }
    }

    #[test]
    fn ambiguous_compiler_artifacts_do_not_select_test_executables() {
        let valid = artifact("/built/x", "lib", "x", true);
        for mut wire in [
            valid.clone(),
            format!("  {valid}"),
            valid.replacen("\"reason\"", r#""\u0072eason""#, 1),
            valid.replacen(
                '{',
                r#"{"extra":[1,-1,1.5,true,false,null,{"text":"ok"}],"#,
                1,
            ),
        ] {
            assert_eq!(take_test_executables(&mut wire).unwrap().len(), 1);
            assert!(wire.is_empty());
        }
        let mut diagnostic = "human diagnostic\n{\"human\":1}\n{\"reason\":\"compiler-message\",\"message\":{}}\n{\"reason\":\"build-finished\",\"success\":true}\n".to_owned();
        assert!(take_test_executables(&mut diagnostic).unwrap().is_empty());
        assert_eq!(diagnostic, "human diagnostic\n{\"human\":1}\n");
        for message in include_str!("testdata/ambiguous-compiler-artifacts.jsonl").lines() {
            for before in [true, false] {
                let valid = artifact("/built/x", "lib", "x", true);
                let wire = if before {
                    format!("{valid}{message}\n")
                } else {
                    format!("{message}\n{valid}")
                };
                let mut output = format!("human diagnostic\n{wire}");
                assert!(take_test_executables(&mut output).is_err(), "{message}");
                assert_eq!(output, "human diagnostic\n");
            }
        }
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
                harness: false,
                exe: "/r/target/debug/deps/aterm_link-1".into(),
                flag: "--lib".into(),
                bin_env: Vec::new(),
            },
            TestExe {
                harness: false,
                exe: "/r/target/debug/deps/bridge_e2e-2".into(),
                flag: "--test bridge_e2e".into(),
                bin_env: Vec::new(),
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
                harness: false,
                exe: Path::new("/cache/r/.cargo-build/debug/deps")
                    .join(e.exe.file_name().expect("a file name")),
                flag: e.flag.clone(),
                bin_env: Vec::new(),
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
                harness: false,
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let runs =
            run_all(
                &bins,
                3,
                12,
                |b, threads| {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    assert_eq!(threads, 12, "every binary gets the whole count");
                    std::thread::sleep(std::time::Duration::from_millis(
                        if b.spec.ends_with('0') { 60 } else { 5 },
                    ));
                    running.fetch_sub(1, Ordering::SeqCst);
                    Run {
                        stopped_at: None,
                        ok: true,
                        output: format!("{}\nran {}\n", b.header, b.spec),
                        code: Some(0),
                        spawn_error: None,
                        usage: None,
                    }
                },
                |_, _, _, _| {},
            );
        assert!(peak.load(Ordering::SeqCst) <= 3, "never past the jobs");
        let said: Vec<String> = runs
            .iter()
            .map(|(r, _)| r.output.lines().nth(1).unwrap().to_string())
            .collect();
        assert_eq!(
            said,
            (0..9)
                .map(|i| format!("ran -p x --test t{i}"))
                .collect::<Vec<_>>(),
            "declared order"
        );
    }

    /// THE LONG POLES: the run's own wall, then its slowest binaries longest
    /// first, at most [`POLES_SHOWN`] of them, each by cargo's re-run spec.
    #[test]
    fn the_long_poles_name_the_slowest_binaries_longest_first() {
        let secs = [3, 41, 0, 7, 12, 1, 2, 5, 9, 4];
        let bins: Vec<Binary> = (0..secs.len())
            .map(|i| Binary {
                harness: false,
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let walls: Vec<Duration> = secs.iter().map(|&s| Duration::from_secs(s)).collect();
        let line = poles(&bins, &walls, Duration::from_secs(44));
        assert!(line.contains("the run took 44s"), "{line}");
        let at = |i: usize| line.find(&format!("--test t{i} ")).map(|p| (p, i));
        let named: Vec<usize> = {
            let mut found: Vec<(usize, usize)> = (0..secs.len()).filter_map(at).collect();
            found.sort_unstable();
            found.into_iter().map(|(_, i)| i).collect()
        };
        assert_eq!(named, [1, 4, 8, 3, 7, 9, 0, 6], "{line}");
    }

    /// A BINARY THE RUN'S CEILING LEFT NO TIME TO START is a FAIL in cargo's
    /// shape — its header, the run's TIMEOUT words, cargo's re-run
    /// line — one finding for the row, which says so.
    #[test]
    fn a_binary_the_ceiling_left_no_time_for_is_a_fail() {
        let b = Binary {
            harness: false,
            header: "     Running tests/late.rs (target/debug/deps/late-1)".into(),
            spec: "-p x --test late".into(),
            record: rec("/r/late", "x"),
        };
        let run = not_started(&b, Duration::from_secs(10_800));
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
                harness: false,
                header: format!("     Running tests/{n}.rs (target/debug/deps/{n}-1)"),
                spec: format!("-p x --test {n}"),
                record: rec(&format!("/r/target/debug/deps/{n}-1"), "x"),
            })
            .collect();
        let pass = |b: &Binary| Run {
            stopped_at: None,
            ok: true,
            output: format!(
                "{}\n\nrunning 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 \
                 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
                b.header
            ),
            code: Some(0),
            spawn_error: None,
            usage: None,
        };
        let fail = |b: &Binary| Run {
            stopped_at: None,
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
            usage: None,
        };
        let runs = [pass(&bins[0]), fail(&bins[1]), pass(&bins[2])];
        let all = combined(&bins, &runs);
        assert!(!all.ok);
        let tests = crate::libtest::failed_tests(&all.output).expect("accounted for");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].spec, "-p x --test b");
        assert_eq!(tests[0].name, "t::broke");
        let (outcome, event, _) = binary_event(
            &bins[1],
            &runs[1],
            true,
            Duration::ZERO,
            Duration::ZERO,
            1,
            None,
        );
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed), "a proven red");
        let event: aterm_json::Value =
            aterm_json::from_str(event.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["failures"][0]["case"].as_str(), Some("t::broke"));
        assert_eq!(
            event["failures"][0]["output"].as_str(),
            Some(tests[0].block.as_str())
        );
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
            stopped_at: None,
            ok: false,
            output: format!("{}\n\nrunning 1 test\n", bins[1].header),
            code: None,
            spawn_error: None,
            usage: None,
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
            stopped_at: None,
            ok: false,
            output: fail(&bins[1])
                .output
                .replace("assertion failed: false", &format!("{sentinel} — strays")),
            code: Some(101),
            spawn_error: None,
            usage: None,
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

    /// A test executable runs with its own package's binaries, named as Cargo names them while
    /// it compiles the test (`CARGO_BIN_EXE_<name>`), which a test reads at run time
    /// (`aterm_tempfile::bin_exe`); a value its record already holds gives way. NEGATIVE CONTROL:
    /// package y's binary reaches no test of package x, and x's none of y's.
    #[test]
    fn a_test_executable_runs_with_its_packages_binaries() {
        let of = |pkg: &str, exe: &str, kind: &str, name: &str, test: bool| {
            artifact(exe, kind, name, test).replace("/r/crates/x#", &format!("/r/crates/{pkg}#"))
        };
        let mut log = [
            of("x", "/r/target/debug/deps/it-1", "test", "it", true),
            of("x", "/r/target/debug/x-tool", "bin", "x-tool", false),
            of("y", "/r/target/debug/y-tool", "bin", "y-tool", false),
            of("y", "/r/target/debug/deps/yt-2", "test", "yt", true),
        ]
        .concat();
        let exes = take_test_executables(&mut log).expect("readable");
        let named = |env: &[(OsString, OsString)]| {
            env.iter()
                .filter(|(k, _)| k.to_string_lossy().starts_with("CARGO_BIN_EXE_"))
                .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            named(&exes[0].bin_env),
            ["CARGO_BIN_EXE_x-tool=/r/target/debug/x-tool"]
        );
        assert_eq!(
            named(&exes[1].bin_env),
            ["CARGO_BIN_EXE_y-tool=/r/target/debug/y-tool"]
        );
        let mut record = rec("/r/target/debug/deps/it-1", "x");
        record
            .env
            .push(("CARGO_BIN_EXE_x-tool".into(), "/stale/x-tool".into()));
        let header = "     Running tests/it.rs (target/debug/deps/it-1)\n";
        let bins = plan(vec![record], header, &exes).expect("a plan");
        assert_eq!(
            named(&binary_cmd(&bins[0], 1, false, Watch::default()).envs),
            ["CARGO_BIN_EXE_x-tool=/r/target/debug/x-tool"],
            "the compile's binary, once, in place of the stale one"
        );
    }

    /// A binary verify ended unrun (or could not start) is its own COULD NOT RUN row, naming its
    /// spec, and the rest decide the run's row as before: a pass beside it and a failed test
    /// itemized under cargo's rerun spec. The combined log keeps its output and names it no
    /// failed target. Negative control: a binary killed by a signal with no `spawn_error` is
    /// still part of the one finding. With every binary unrun, only their rows; with no binary at
    /// all (a resume carried every one), the run's one row as before.
    #[test]
    fn a_binary_verify_ended_unrun_is_its_own_could_not_run_row() {
        let bins: Vec<Binary> = ["a", "b", "c"]
            .iter()
            .map(|n| Binary {
                harness: false,
                header: format!("     Running tests/{n}.rs (target/debug/deps/{n}-1)"),
                spec: format!("-p x --test {n}"),
                record: rec(&format!("/r/target/debug/deps/{n}-1"), "x"),
            })
            .collect();
        let pass = |b: &Binary| Run {
            stopped_at: None,
            ok: true,
            output: format!(
                "{}\n\nrunning 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 \
                 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
                b.header
            ),
            code: Some(0),
            spawn_error: None,
            usage: None,
        };
        let fail = |b: &Binary| Run {
            stopped_at: None,
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
            usage: None,
        };
        let why = "the run was interrupted before this child started";
        let unrun = |b: &Binary| Run {
            stopped_at: None,
            ok: false,
            output: format!("{}\naterm-verify: cannot run {}: {why}", b.header, b.spec),
            code: None,
            spawn_error: Some(why.to_string()),
            usage: None,
        };
        let label = "targo test --tests";
        let decided = |runs: &[Run]| {
            let mut r = Report::new("test");
            r.raw(combined(&bins, runs).output);
            decide_runs(&mut r, &bins, runs, label, |r, rest| {
                r.decide_test_child(rest, label, &[]);
            });
            r
        };
        let cnr = Outcome::Fail(Severity::CouldNotRun);
        let gf = Outcome::Fail(Severity::GateFailed);

        let r = decided(&[pass(&bins[0]), unrun(&bins[1]), fail(&bins[2])]);
        let rows: Vec<_> = r.outcomes().collect();
        let b_row = format!("{label} — -p x --test b: {why}");
        assert_eq!(rows, [(cnr, b_row.as_str()), (gf, label)], "{}", r.render());
        let ids: Vec<String> = r.findings().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["-p x --test c -- t::broke"], "{}", r.render());
        let block = r.render();
        assert!(
            block.contains("error: 1 target failed:\n    `-p x --test c`\n")
                && !block.contains("rerun pass `-p x --test b`")
                && !block.contains("could not execute"),
            "the log names no unrun binary a failed target:\n{block}"
        );

        let crashed = Run {
            stopped_at: None,
            ok: false,
            output: format!("{}\n\nrunning 1 test\n", bins[1].header),
            code: None,
            spawn_error: None,
            usage: None,
        };
        let r = decided(&[pass(&bins[0]), crashed, fail(&bins[2])]);
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [gf],
            "{}",
            r.render()
        );

        let r = decided(&[unrun(&bins[0]), unrun(&bins[1]), unrun(&bins[2])]);
        assert!(
            r.outcomes()
                .all(|(o, l)| o == cnr && !l.contains(crate::libtest::RAN_NO_TEST)),
            "{}",
            r.render()
        );
        assert_eq!(r.outcomes().count(), 3, "{}", r.render());

        let mut r = Report::new("test");
        decide_runs(&mut r, &[], &[], label, |r, rest| {
            r.decide_test_child(rest, label, &[]);
        });
        assert_eq!(r.outcomes().count(), 1, "{}", r.render());
    }

    /// The compile's artifacts carry their manifest's harness: a `[[test]]` that says
    /// `harness = false` is false and its autodiscovered sibling true, read from the
    /// `manifest_path` each message names. NEGATIVE CONTROL: an artifact with no manifest path
    /// is false.
    #[test]
    fn each_test_executable_carries_its_manifests_harness() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let manifest = dir.path().join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"x\"\n[[test]]\nname = \"custom\"\nharness = false\n",
        )
        .expect("a manifest");
        let with = |line: String| {
            line.replacen(
                "\"reason\":\"compiler-artifact\",",
                &format!(
                    "\"reason\":\"compiler-artifact\",\"manifest_path\":{},",
                    aterm_json::json!(manifest.to_str().expect("utf-8"))
                ),
                1,
            )
        };
        let mut log = [
            with(artifact("/r/t/custom-1", "test", "custom", true)),
            with(artifact("/r/t/plain-1", "test", "plain", true)),
            artifact("/r/t/bare-1", "test", "bare", true),
        ]
        .concat();
        let exes = take_test_executables(&mut log).expect("read");
        assert_eq!(
            exes.iter()
                .map(|e| (e.flag.as_str(), e.harness))
                .collect::<Vec<_>>(),
            [
                ("--test custom", false),
                ("--test plain", true),
                ("--test bare", false)
            ]
        );
    }

    /// A TARGET'S HARNESS IS ITS MANIFEST'S WORD: `harness = false` under `[lib]`, `[[test]]` or
    /// `[[bench]]` keeps libtest's flags from it; an autodiscovered target, and one whose entry
    /// says nothing, runs the harness; a name two entries share, and a manifest that could not be
    /// read, are false, so a JSON flag never reaches a custom main. NEGATIVE CONTROL: the
    /// entries' siblings, harness unsaid, read true.
    #[test]
    fn a_targets_harness_is_its_manifests_word() {
        let manifest = aterm_toml::from_str::<aterm_toml::Value>(
            "[package]\nname = \"x\"\n[lib]\nharness = false\n\
             [[test]]\nname = \"custom\"\nharness = false\n\
             [[test]]\nname = \"plain\"\n\
             [[test]]\npath = \"tests/stem.rs\"\nharness = false\n\
             [[bench]]\nname = \"speed\"\nharness = false\n\
             [[bin]]\nname = \"twice\"\n[[bin]]\nname = \"twice\"\n",
        )
        .expect("a manifest");
        let m = Some(&manifest);
        assert!(!harness_of(m, &["lib"], "x"));
        assert!(!harness_of(m, &["rlib", "lib"], "x"));
        assert!(!harness_of(m, &["test"], "custom"));
        assert!(harness_of(m, &["test"], "plain"));
        assert!(!harness_of(m, &["test"], "stem"));
        assert!(harness_of(m, &["test"], "autodiscovered"));
        assert!(!harness_of(m, &["bench"], "speed"));
        assert!(harness_of(m, &["bench"], "other"));
        assert!(!harness_of(m, &["bin"], "twice"), "an ambiguous name");
        assert!(harness_of(m, &["bin"], "once"));
        assert!(!harness_of(None, &["test"], "plain"), "an unread manifest");
        let bare = aterm_toml::from_str::<aterm_toml::Value>("[package]\nname = \"y\"\n")
            .expect("a manifest");
        assert!(harness_of(Some(&bare), &["lib"], "y"));
    }

    #[test]
    fn the_replayed_command_is_the_record_with_its_thread_count() {
        let mut b = Binary {
            header: "     Running tests/a.rs (target/debug/deps/a-1)".into(),
            spec: "-p x --test a".into(),
            record: rec("/r/target/debug/deps/a-1", "x"),
            harness: true,
        };
        b.record
            .env
            .push((crate::stages::CHECKERS_ENV.into(), "1".into()));
        let cmd = binary_cmd(&b, 7, false, Watch::default());
        let json = binary_cmd(
            &b,
            7,
            false,
            Watch {
                json: true,
                stop_at_red: true,
            },
        );
        assert_eq!(
            json.argv(),
            [
                "/r/target/debug/deps/a-1",
                "-Z",
                "unstable-options",
                "--format",
                "json",
                "--skip",
                "slow::"
            ]
        );
        assert_eq!(
            json.test_cases,
            Some(crate::exec::CaseWatch {
                spec: "-p x --test a".into(),
                json: true,
                stop_at_red: true
            })
        );
        b.harness = false;
        assert_eq!(
            binary_cmd(
                &b,
                7,
                false,
                Watch {
                    json: true,
                    stop_at_red: true
                }
            )
            .argv(),
            cmd.argv(),
            "no JSON flag reaches a custom main"
        );
        b.harness = true;
        let armed = Watch {
            json: true,
            stop_at_red: true,
        };
        for var in ["RUST_TEST_SHUFFLE", "RUST_TEST_SHUFFLE_SEED"] {
            let mut shuffled = b.clone();
            shuffled.record.env.push((var.into(), "1".into()));
            assert!(
                !binary_cmd(&shuffled, 7, false, armed)
                    .argv()
                    .iter()
                    .any(|a| a == "-Z"),
                "{var} is read only once -Z unstable-options is passed"
            );
        }
        let mut unstable = b.clone();
        unstable.record.argv.push("-Zunstable-options".into());
        unstable.record.argv.push("--shuffle".into());
        assert_eq!(
            binary_cmd(&unstable, 7, false, armed).argv().len(),
            unstable.record.argv.len(),
            "a record that already passes -Z keeps its own flags alone"
        );
        for required in [false, true] {
            let cmd = binary_cmd(&b, 7, required, Watch::default());
            let policy: Vec<_> = cmd
                .envs
                .iter()
                .filter(|(k, _)| k == crate::stages::CHECKERS_ENV)
                .map(|(_, v)| v.to_str().unwrap())
                .collect();
            assert_eq!(policy, [if required { "1" } else { "0" }]);
        }
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

    #[test]
    fn plan_prefers_exact_exe_match_before_file_name_fallback() {
        let records = vec![rec("/r/target/debug/deps/a-1", "x")];
        let log = "     Running tests/a.rs (target/debug/deps/a-1)\n";
        let exes = vec![
            TestExe {
                harness: false,
                exe: PathBuf::from("/other/target/debug/deps/a-1"),
                flag: "--lib".to_string(),
                bin_env: Vec::new(),
            },
            TestExe {
                harness: false,
                exe: PathBuf::from("/r/target/debug/deps/a-1"),
                flag: "--test a".to_string(),
                bin_env: Vec::new(),
            },
        ];
        let bins = plan(records, log, &exes).expect("plan");
        assert_eq!(bins[0].spec, "-p x --test a");
    }

    #[test]
    fn record_decode_rejects_duplicate_cwd() {
        let mut bytes = rec("/r/target/debug/deps/a-1", "x").encode();
        bytes.extend_from_slice(b"cwd\0/other\0");
        assert!(Record::decode(&bytes).is_none());
    }

    /// Pool layout initialized by `testrun` matches `aterm_spec::subprocess_lane`'s file-lock
    /// protocol and bounds concurrent exclusive leases across callers.
    /// NEGATIVE CONTROL: a third thread contending on a two-slot `testrun` pool cannot acquire an
    /// exclusive slot before its timeout while two holders remain active.
    #[cfg(any(unix, windows))]
    #[test]
    fn subprocess_pool_protocol_matches_aterm_spec_lane() {
        use aterm_spec::subprocess_lane;

        assert_eq!(SUBPROCESS_SLOTS_ENV, subprocess_lane::SUBPROCESS_SLOTS_ENV);
        for cores in [0, 1, 2, 5, 6, 9, 14, 32] {
            let slots = subprocess_slots_for_cores(cores);
            assert_eq!(slots, subprocess_lane::slots_for_cores(cores));
            assert_eq!(
                subprocess_boot_slots(slots),
                subprocess_lane::boot_slots_for_pool(slots)
            );
        }

        let tmp = aterm_tempfile::tempdir().expect("scratch pool");
        let dir = tmp.path().join("pool");
        std::fs::create_dir(&dir).expect("owned pool directory");
        init_subprocess_pool_dir(&dir, 2).expect("init 2-slot pool");
        assert!(
            init_subprocess_pool_dir(&dir, 2).is_err(),
            "a populated fixed layout must not be rewritten"
        );

        let (release_a, wait_a) = std::sync::mpsc::channel::<()>();
        let (release_b, wait_b) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();

        std::thread::scope(|scope| {
            let dir_ref = &dir;
            let ready_tx_a = ready_tx.clone();
            let h_a = scope.spawn(move || {
                let _lease =
                    subprocess_lane::acquire_with_pool(Some(dir_ref), Duration::from_secs(60))
                        .expect("stack admission");
                let _boot = subprocess_lane::acquire_boot_permit_with_pool(
                    Some(dir_ref),
                    Duration::from_secs(60),
                )
                .expect("boot admission");
                ready_tx_a.send(true).expect("send a");
                let _ = wait_a.recv();
            });
            let h_b = scope.spawn(move || {
                let lease =
                    subprocess_lane::acquire_with_pool(Some(dir_ref), Duration::from_secs(60));
                ready_tx.send(lease.is_ok()).expect("send b");
                let _ = wait_b.recv();
            });
            assert!(
                ready_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("a acquired")
            );
            assert!(
                ready_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("b acquired")
            );

            let third = scope
                .spawn(move || {
                    subprocess_lane::acquire_with_pool(Some(dir_ref), Duration::ZERO)
                        .err()
                        .map(|e| e.kind())
                })
                .join()
                .expect("third joined");
            assert_eq!(third, Some(std::io::ErrorKind::TimedOut));

            drop(release_a);
            drop(release_b);
            h_a.join().expect("join a");
            h_b.join().expect("join b");
        });
    }
}
