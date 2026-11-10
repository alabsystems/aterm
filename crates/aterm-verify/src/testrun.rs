// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running test binaries concurrently: each at the width its runs call for, as many at once as
//! their charges fit in the run's CPU share ([`crate::test_plan`]).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::exec::{self, Cmd, Run};
use crate::ladder::{Outcome, Report, Severity};
use crate::test_plan::{self, Dispatcher, Plan, Planned};
use crate::test_progress::{Phase, Progress};

pub use crate::test_content::{CarryProof, Evidence, Passed, encode_passed, read_passed};

/// The flag cargo's runner config hands this binary.
pub const RECORD_FLAG: &str = "--record-test-binary";

/// How long a binary the live-rate backstop holds back waits before it looks again, unless a
/// binary ends first: the stage meter's read cadence, which moves the rate.
const LOOK_AGAIN: Duration = exec::READ_USAGE_EVERY;

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
pub fn runner_config(gate: &Path, dir: &Path, host: &str) -> Option<String> {
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
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(format!(
        "target.{host}.runner=[{},{},{}]",
        q(gate.to_str()?),
        q(RECORD_FLAG),
        q(dir.to_str()?)
    ))
}

/// Build the serial `--tests` command with the exact-host recording runner. The inherited target
/// and exact-host runner variables are removed after [`configured_build_targets`] proved Cargo
/// has no configured target; otherwise Cargo could execute a runnable non-host binary outside
/// [`exec::CaseWatch`].
#[must_use]
pub fn recording_cmd(serial: &Cmd, config: &str, host: &str) -> Cmd {
    let runner_env = format!(
        "CARGO_TARGET_{}_RUNNER",
        host.to_ascii_uppercase().replace(['-', '.'], "_")
    );
    let mut c = crate::compile_account::with_message_format(
        serial
            .clone()
            .remove_env("CARGO_BUILD_TARGET")
            .remove_env(runner_env),
    );
    c.args
        .splice(0..0, [OsString::from("--config"), OsString::from(config)]);
    c.args = aterm_toolchain::build_cache::configs_after_subcommand(&c.args);
    c
}

/// Cargo's effective `build.target`, read through Cargo itself so environment variables,
/// hierarchical config and `$CARGO_HOME` have exactly Cargo's precedence. An explicitly selected
/// target is refused by the native test dispatcher: it changes the artifact layout and could
/// escape the exact-host recorder. `None` is Cargo's native default.
fn configured_build_targets(ctx: &crate::Ctx, serial: &Cmd) -> Result<Option<Vec<String>>, String> {
    let mut configs = Vec::new();
    let mut explicit_target = None;
    let mut args = serial.args.iter().peekable();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--config" {
            let value = args
                .next()
                .ok_or_else(|| "Cargo's --config has no value".to_owned())?;
            configs.extend([arg.clone(), value.clone()]);
            continue;
        }
        let text = arg.to_string_lossy();
        if text.starts_with("--config=") {
            configs.push(arg.clone());
        } else if text == "--target" {
            explicit_target = Some(
                args.next()
                    .ok_or_else(|| "Cargo's --target has no value".to_owned())?
                    .to_string_lossy()
                    .into_owned(),
            );
        } else if let Some(target) = text.strip_prefix("--target=") {
            explicit_target = Some(target.to_owned());
        }
    }
    if let Some(target) = explicit_target {
        if target.trim().is_empty() {
            return Err("Cargo's --target is empty".to_owned());
        }
        return Ok(Some(vec![target]));
    }
    let mut query = serial.clone();
    query.args = [
        "-Z",
        "unstable-options",
        "config",
        "get",
        "build.target",
        "--format",
        "json",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    query.args.extend(configs);
    query.args = aterm_toolchain::build_cache::configs_after_subcommand(&query.args);
    query.capture = exec::Capture::Emit;
    query.preamble = None;
    query.test_cases = None;
    let exec::MachineRun { run, stdout } = exec::run_machine(&query, ctx.exec_env(), None);
    if !run.ok {
        if run
            .output
            .contains("config value `build.target` is not set")
        {
            return Ok(None);
        }
        return Err(format!(
            "cannot read Cargo's effective build.target: {}",
            first_error(&run.output)
        ));
    }
    let stdout = stdout.map_err(|why| format!("cannot read Cargo's build.target answer: {why}"))?;
    let document: aterm_json::Value = aterm_json::from_str(&stdout)
        .map_err(|error| format!("Cargo's build.target query did not print JSON: {error}"))?;
    let value = document
        .get("build")
        .and_then(|build| build.get("target"))
        .ok_or_else(|| "Cargo's build.target query omitted build.target".to_owned())?;
    let targets = if let Some(target) = value.as_str() {
        vec![target.to_owned()]
    } else {
        value
            .as_array()
            .ok_or_else(|| "Cargo's build.target is neither a string nor an array".to_owned())?
            .iter()
            .map(|target| {
                target
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "Cargo's build.target array contains a non-string".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if targets.is_empty() || targets.iter().any(|target| target.trim().is_empty()) {
        return Err("Cargo's build.target query returned an empty target".to_owned());
    }
    Ok(Some(targets))
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
            crate::compiler_message::UnitMessage::Ambiguous { .. } => {
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

/// One binary's run ([`dispatch`]).
#[derive(Clone, Debug)]
pub struct Ran {
    /// How it ended, with what its tree used ([`Run::usage`]).
    pub run: Run,
    /// Its width and charge.
    pub planned: Planned,
    /// From the dispatch's start to its own.
    pub waiting: Duration,
    /// Its own wall.
    pub execution: Duration,
}

impl Ran {
    /// The cores it used on average, where it was read.
    #[must_use]
    pub fn cores(&self) -> Option<f64> {
        let wall = self.execution.as_secs_f64();
        let used = self.run.usage?.cpu.as_secs_f64() / wall;
        (wall > 0.0 && used.is_finite()).then_some(used)
    }

    /// The cores it used on average beyond its charge, where it used more: its charge rises by
    /// as much next run ([`crate::test_history::History::seen`]).
    #[must_use]
    pub fn overran(&self) -> Option<f64> {
        self.cores()
            .filter(|&used| used > f64::from(self.planned.charge))
    }
}

/// Run every binary of `plan` at its width, starting each as [`Dispatcher`] admits it: in the
/// plan's order, once its charge fits in what the running binaries leave of the plan's share and
/// its peak in what they leave of `memory`, with the running binaries using `live()` cores at
/// their last reads. Once `stop()` (the run's wall-clock ceiling is spent, or its run-wide halt is
/// armed) it halts and starts nothing more; `completed` is handed a local halt that takes the
/// same lock as admission, so a decisive red can close dispatch before it writes its event. A
/// binary `run_one` answers `None` for did not start (the run stopped as its turn came) is answered
/// as one the halt left. Each worker books
/// its children into the calling stage's memory meter ([`crate::memory::current`]), hands them
/// its cover marker ([`crate::memory::cover`]) and retains the calling thread's memory
/// reservation, so nothing the stage claimed is released while a binary runs. `completed` hears
/// each binary as it ends, before its charge goes back. A binary's charge goes back however its
/// turn ends ([`Charged`]): one whose `run_one` or `completed` panicked halts the dispatch, and
/// the panic reaches the caller once the running binaries end. The answers are in `bins`' order;
/// `None` for a binary the halt left unstarted.
pub fn dispatch<F, E>(
    bins: &[Binary],
    plan: &Plan,
    memory: Option<u64>,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    run_one: F,
    completed: E,
) -> Vec<Option<Ran>>
where
    F: Fn(&Binary, &Planned, &(dyn Fn() + Sync)) -> Option<Run> + Sync,
    E: Fn(&Binary, &Ran, &dyn Fn()) + Sync,
{
    dispatch_pooled(bins, plan, memory, None, live, stop, run_one, completed)
}

/// [`dispatch`], each started binary also holding its charge in the release's `pool`
/// ([`aterm_capacity::POOL_ENV`], [`crate::Ctx::pool`]) while it runs, where one was handed.
///
/// THE CHARGE IN TOKENS. The plan's share is the pool's slots, so the charges the dispatcher
/// admits never pass them; the pool's other holders (the release's build and export) are not in
/// the dispatcher's books, so a binary the share admits also takes its charge's tokens
/// ([`aterm_capacity::Jobserver::acquire_many`], one partial holder at a time) before its clock
/// starts, its wait queue time ([`Ran::waiting`]), and gives them back as its process ends,
/// before `completed` and before its share. The tokens the running binaries hold are thus their
/// charges, and with the pool's other holders never more than its slots. A wait `stop()` ends
/// leaves the binary unstarted, as the stop leaves every other; one an interrupt or the FIFO
/// ends answers it as a binary verify could not start ([`Run::spawn_error`]), never run.
#[expect(
    clippy::too_many_arguments,
    reason = "dispatch's own seven inputs and the release's pool, which only a run under a \
              release hands"
)]
pub fn dispatch_pooled<F, E>(
    bins: &[Binary],
    plan: &Plan,
    memory: Option<u64>,
    pool: Option<&aterm_capacity::Jobserver>,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    run_one: F,
    completed: E,
) -> Vec<Option<Ran>>
where
    F: Fn(&Binary, &Planned, &(dyn Fn() + Sync)) -> Option<Run> + Sync,
    E: Fn(&Binary, &Ran, &dyn Fn()) + Sync,
{
    let unpaid = || stop() || exec::ending();
    let answers: Vec<Mutex<Option<Ran>>> = bins.iter().map(|_| Mutex::new(None)).collect();
    let state = Mutex::new(Dispatcher::new(plan, memory));
    let turn = Condvar::new();
    let halt = || halt_dispatch(&state, &turn);
    let meter = crate::memory::current();
    let cover = crate::memory::cover();
    let reservation = crate::current_resource_claim();
    let queued = Instant::now();
    // Each running binary holds at least one core of the share, so no more run at once.
    let workers = usize::try_from(plan.share)
        .unwrap_or(usize::MAX)
        .min(plan.order.len())
        .max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let _metering = crate::memory::metering(meter.clone());
                let _covering = crate::memory::covering(cover.clone());
                let _reservation = crate::holding_resource_claim(reservation.clone());
                while let Some(b) = admitted(&state, &turn, live, stop) {
                    let _charged = Charged {
                        state: &state,
                        turn: &turn,
                        index: b.index,
                    };
                    // The next may fit beside it.
                    turn.notify_all();
                    if let Some(binary) = bins.get(b.index) {
                        let tokens = pool.map(|pool| pool.acquire_many(b.charge, &unpaid));
                        let (tokens, refused) = match tokens {
                            None => (None, None),
                            Some(Ok(tokens)) => (Some(tokens), None),
                            // The stop leaves it unstarted, as it leaves the rest.
                            Some(Err(_)) if stop() => continue,
                            Some(Err(why)) => (None, Some(unpaid_run(&why))),
                        };
                        let started = Instant::now();
                        let answered = refused.or_else(|| run_one(binary, &b, &halt));
                        drop(tokens);
                        if let Some(run) = answered {
                            let ran = Ran {
                                run,
                                planned: b,
                                waiting: started.duration_since(queued),
                                execution: started.elapsed(),
                            };
                            completed(binary, &ran, &halt);
                            *answers[b.index]
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) = Some(ran);
                        }
                    }
                }
            });
        }
    });
    answers
        .into_iter()
        .map(|a| a.into_inner().unwrap_or_else(PoisonError::into_inner))
        .collect()
}

/// A binary whose wait for the release's pool's tokens an interrupt or the FIFO ended: verify
/// could not start it, and it never ran.
fn unpaid_run(why: &std::io::Error) -> Run {
    let why = if exec::ending() {
        "the run was interrupted before this child started".to_owned()
    } else {
        format!("the release's pool gave it no tokens: {why}")
    };
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: Some(why),
        usage: None,
    }
}

/// Close a dispatch and wake every admission waiter. This takes the same mutex as a start, so a
/// halt that returns is ordered before every later start attempt.
fn halt_dispatch(state: &Mutex<Dispatcher>, turn: &Condvar) {
    state.lock().unwrap_or_else(PoisonError::into_inner).halt();
    turn.notify_all();
}

/// A started binary's charge, held until its turn ends, however it ends: dropped, it gives the
/// charge and the peak back and wakes the workers waiting for them. A turn a panic ended halts
/// the dispatch first, so no worker waits on a share the panicking one never gives back, and the
/// scope's join hands the panic to the caller once the running binaries end.
struct Charged<'a> {
    state: &'a Mutex<Dispatcher>,
    turn: &'a Condvar,
    index: usize,
}

impl Drop for Charged<'_> {
    fn drop(&mut self) {
        let mut d = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if std::thread::panicking() {
            d.halt();
        }
        // It was started, so it ends once: nothing refuses this.
        d.finish(self.index).ok();
        drop(d);
        self.turn.notify_all();
    }
}

/// The next binary the dispatcher admits, waiting for one to end or the live rate to move;
/// `None` once none is left to start or the run halted.
fn admitted(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Planned> {
    admitted_after_stop(state, turn, live, stop, &|| {})
}

/// [`admitted`], with a test seam after its first stop read and before it takes the dispatcher's
/// mutex. A local red takes that mutex through [`halt_dispatch`], so a stop committed in this gap
/// is visible before the start attempt.
fn admitted_after_stop(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
) -> Option<Planned> {
    loop {
        let stopping = stop();
        after_stop();
        let mut d = state.lock().unwrap_or_else(PoisonError::into_inner);
        if stopping || stop() {
            d.halt();
        }
        if !d.waiting() {
            return None;
        }
        if let Some(b) = d.next(live()) {
            return Some(b);
        }
        drop(match turn.wait_timeout(d, LOOK_AGAIN) {
            Ok((d, _)) => d,
            Err(poisoned) => poisoned.into_inner().0,
        });
    }
}

/// The most bytes a `test-binary-finished` event spends on each failed case's output: with at
/// most [`crate::report::FINDINGS_SHOWN`] cases the line stays far under the 64 KiB the release's
/// reader holds a line to, so a FAIL is never lost to its own length.
pub const FAILURE_OUTPUT_BYTES: usize = 2048;
/// The most unfinished case names one hung-binary event carries. Eight leaves room under the
/// release reader's 64 KiB line cap beside ten full failure blocks and the hang detail.
const HUNG_CASES_SHOWN: usize = 8;

/// What a release's resume does with its binaries ([`carry`]).
pub struct Carried {
    /// The binaries that run, in their order.
    pub run: Vec<Binary>,
    /// The binaries carried, with current evidence and the prior execution wall.
    pub carried: Vec<CarriedBinary>,
    /// The current evidence of every release binary, by spec.
    pub known: BTreeMap<String, Evidence>,
}

/// A prior success reused without executing this hop.
pub struct CarriedBinary {
    pub binary: Binary,
    pub evidence: Evidence,
    /// The prior run's global libtest JSON decision, which is part of the reconstructed command.
    pub json: bool,
    pub prior_execution_s: f64,
}

/// Which of `bins` a release carries. `evidence` runs for every binary, including a first run
/// with an empty `passed` set. A binary is carried only when its prior outcome was `ok` (enforced
/// by [`read_passed`]), both input sets are bounded, executable and complete content key match,
/// and its package is not [`crate::checks::GOLDEN`]'s. The rest run, in their original order.
pub fn carry(
    passed: &Passed,
    bins: Vec<Binary>,
    evidence: impl Fn(&Binary, Option<&CarryProof>) -> Evidence + Sync,
    jobs: usize,
) -> Carried {
    let golden = |b: &Binary| {
        b.record.var("CARGO_PKG_NAME").and_then(OsStr::to_str) == Some(crate::checks::GOLDEN.0)
    };
    let asked: Vec<usize> = (0..bins.len()).collect();
    let evidence = pool(bins.len(), &asked, jobs, |i| {
        evidence(&bins[i], passed.get(&bins[i].spec))
    });
    let mut known = BTreeMap::new();
    let (mut run, mut carried) = (Vec::new(), Vec::new());
    for (binary, evidence) in bins.into_iter().zip(evidence) {
        let evidence = evidence.expect("every release binary was assigned to the evidence pool");
        known.insert(binary.spec.clone(), evidence.clone());
        let prior = passed.get(&binary.spec);
        let matches = !golden(&binary)
            && evidence.bounded()
            && prior.is_some_and(|prior| {
                prior.outcome == "ok"
                    && prior.inputs_bounded
                    && prior.exe_sha256 == evidence.exe_sha256
                    && prior.content_key == evidence.content_key
            });
        if matches {
            carried.push(CarriedBinary {
                json: prior.expect("matched prior proof").json,
                prior_execution_s: prior.expect("matched prior proof").execution_s,
                binary,
                evidence,
            });
        } else {
            run.push(binary);
        }
    }
    Carried {
        run,
        carried,
        known,
    }
}

/// The `test-binary-finished` event of a binary a resume carried: its prior execution passed with
/// exactly this executable and complete bounded runtime-input key. It runs no time in this hop,
/// while `recorded_execution_s` preserves the prior real wall for the next hop and accounting.
fn carried_event(carried: &CarriedBinary) -> String {
    let binary = &carried.binary;
    let evidence = &carried.evidence;
    format!(
        "verify-event: {}",
        aterm_json::json!({
            "schema": 1, "event": "test-binary-finished", "test": binary.spec,
            "outcome": "ok", "exit_code": aterm_json::Value::Null,
            "waiting_s": 0.0, "execution_s": 0.0,
            "cpu_s": aterm_json::Value::Null, "starved_s": aterm_json::Value::Null,
            "peak_bytes": aterm_json::Value::Null,
            "width": aterm_json::Value::Null, "tests": aterm_json::Value::Null,
            "test_counts": aterm_json::Value::Null,
            "failures": [],
            "detail": "carried: prior ok with equal executable and complete runtime inputs",
            "evidence": {
                "schema": crate::test_content::SCHEMA,
                "exe_sha256": evidence.exe_sha256,
                "content_key": evidence.content_key,
                "inputs_bounded": evidence.bounded(),
                "input_reason": evidence.reason,
            },
            "exe_sha256": evidence.exe_sha256,
            "content_key": evidence.content_key,
            "inputs_bounded": evidence.bounded(),
            "input_reason": evidence.reason,
            "json": carried.json,
            "recorded_execution_s": carried.prior_execution_s,
            "carried": true,
        })
    )
}

/// A candidate which matched before dispatch but did not remain equal through the whole run.
/// It never executed, so it is NOT RUN rather than a failure. This later row supersedes any
/// older candidate for the spec and makes the test check incomplete instead of inheriting OK.
fn unstable_carried_event(carried: &CarriedBinary, evidence: &Evidence) -> String {
    let binary = &carried.binary;
    format!(
        "verify-event: {}",
        aterm_json::json!({
            "schema": 1, "event": "test-binary-finished", "test": binary.spec,
            "outcome": "NOT RUN", "exit_code": aterm_json::Value::Null,
            "waiting_s": 0.0, "execution_s": 0.0,
            "cpu_s": aterm_json::Value::Null, "starved_s": aterm_json::Value::Null,
            "peak_bytes": aterm_json::Value::Null,
            "width": aterm_json::Value::Null, "tests": aterm_json::Value::Null,
            "test_counts": aterm_json::Value::Null,
            "failures": [],
            "detail": "not carried: runtime inputs changed after carry selection",
            "evidence": {
                "schema": crate::test_content::SCHEMA,
                "exe_sha256": evidence.exe_sha256,
                "content_key": evidence.content_key,
                "inputs_bounded": false,
                "input_reason": evidence.reason,
            },
            "exe_sha256": evidence.exe_sha256,
            "content_key": aterm_json::Value::Null,
            "inputs_bounded": false,
            "input_reason": evidence.reason,
            "json": carried.json,
            "recorded_execution_s": carried.prior_execution_s,
            "carried": false,
        })
    )
}

/// Write one structured line to stderr in one `write_all` of the line and its newline
/// (`eprintln!` writes the newline apart), and to a release's events file, where it lands whole
/// whatever else writes ([`Ctx::record_event`]): stderr is shared with every child, and a line
/// longer than a pipe writes at once can be split there.
fn emit(ctx: &crate::Ctx, event: &str) {
    crate::event_log::emit(ctx.events.as_ref(), event);
}

/// A finished binary's outcome (ok, NOT RUN when its machine or the run's end kept it from
/// running, else FAIL, the proven red that ends a halted run), its `test-binary-finished` event,
/// and the sample of it its history keeps ([`crate::test_history`]) when its run measured what it
/// takes ([`measured`]; `started`: the run left it time to start): its outcome, its waits, its
/// tree's usage (`null` where unread), the test threads it ran at (`width`) and the cores of the
/// share it was charged (`charge`, [`crate::test_plan`]), and the tests libtest said it runs.
fn binary_event(
    binary: &Binary,
    ran: &Ran,
    started: bool,
    evidence: Option<&Evidence>,
    json: bool,
) -> (Outcome, String, Option<crate::test_history::Sample>) {
    let run = &ran.run;
    let width = ran.planned.width;
    // The child has no cargo rerun footer. Classify it with the same known-binary
    // context as the final report, so passing fixture output cannot own a failure.
    let combined =
        (!run.ok).then(|| combined(std::slice::from_ref(binary), std::slice::from_ref(run)));
    let human_failures = || {
        combined
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
            .collect::<Vec<_>>()
    };
    let failures = match &run.case_observations {
        Some(observed) if observed.json => observed
            .failures
            .iter()
            .map(|failed| {
                aterm_json::json!({
                    "case": crate::report::clip_json(&failed.case, crate::report::EVENT_ROW_BYTES),
                    "output": failed.output, "omitted_bytes": failed.omitted_bytes,
                    "refused": failed.marks.refused,
                    "source_path": failed.source_path, "start_line": failed.start_line,
                    "start_col": failed.start_col,
                })
            })
            .collect::<Vec<_>>(),
        _ => human_failures(),
    };
    let unavailable = if run.ok || run.spawn_error.is_some() {
        run.environment_failure()
    } else {
        combined.as_ref().and_then(Run::environment_failure)
    };
    let outcome = if run.ok {
        Outcome::Ok
    } else if run.stopped_at.is_some() || run.hung.is_some() {
        // A decisive red case or a proven hang is a FAIL by construction.
        Outcome::Fail(Severity::GateFailed)
    } else if unavailable.is_some() {
        Outcome::Fail(Severity::CouldNotRun)
    } else {
        Outcome::Fail(Severity::GateFailed)
    };
    let tests = crate::libtest::tests_run(&run.output);
    let test_counts =
        crate::libtest::test_counts(&run.output).filter(|counts| run.ok == (counts.failed == 0));
    let case_timings_complete = run.case_observations.as_ref().is_some_and(|observed| {
        observed.json
            && observed.read_end.is_eof()
            && test_counts.as_ref().is_some_and(|counts| {
                counts.passed.checked_add(counts.failed)
                    == u64::try_from(observed.timings.len()).ok()
            })
    });
    let learned_partition = (run.ok && case_timings_complete)
        .then(|| {
            run.case_observations
                .as_ref()
                .and_then(|observed| crate::test_shards::learn(&observed.timings, ran.execution))
        })
        .flatten();
    let partition_work = run
        .case_observations
        .as_ref()
        .zip(learned_partition.as_deref())
        .map(|(observed, filter)| {
            let (mut selected, mut remaining) = (Duration::ZERO, Duration::ZERO);
            let (mut selected_cases, mut remaining_cases) = (0_usize, 0_usize);
            for timing in &observed.timings {
                if timing.case.contains(filter) {
                    selected = selected.saturating_add(timing.execution);
                    selected_cases += 1;
                } else {
                    remaining = remaining.saturating_add(timing.execution);
                    remaining_cases += 1;
                }
            }
            aterm_json::json!({"filter":filter, "selected_case_s":selected.as_secs_f64(),
            "remaining_case_s":remaining.as_secs_f64(), "selected_cases":selected_cases,
            "remaining_cases":remaining_cases})
        });
    let usage = run.usage;
    let hung_cases = run.hung.as_ref().map(|hung| {
        hung.running
            .iter()
            .take(HUNG_CASES_SHOWN)
            .map(|case| crate::report::clip_json(case, crate::report::EVENT_ROW_BYTES))
            .collect::<Vec<_>>()
    });
    let hung_cases_omitted = run
        .hung
        .as_ref()
        .map(|hung| hung.running.len().saturating_sub(HUNG_CASES_SHOWN));
    let hung_binary = run
        .hung
        .as_ref()
        .map(|hung| crate::report::clip_json(&hung.binary, crate::report::EVENT_ROW_BYTES));
    let stack_sample = run
        .hung
        .as_ref()
        .and_then(|hung| hung.sample.report.as_ref())
        .map(|path| {
            crate::report::clip_json(&path.display().to_string(), crate::report::EVENT_ROW_BYTES)
        });
    let stack_sample_error = run
        .hung
        .as_ref()
        .and_then(|hung| hung.sample.error.as_ref())
        .map(|error| crate::report::clip_json(error, crate::report::EVENT_DETAIL_BYTES));
    let event = format!(
        "verify-event: {}",
        aterm_json::json!({
            "schema": 1, "event": "test-binary-finished",
            "slow_cases": run.case_observations.as_ref().map_or_else(Vec::new,
                |observed| crate::libtest_json::slow_cases(&observed.timings)),
            "case_timings_complete": case_timings_complete,
            "case_timings_observed": run.case_observations.as_ref().map_or(0, |observed| observed.timings.len()),
            "partition": partition_work,
            "test": crate::report::clip_json(&binary.spec, crate::report::EVENT_ROW_BYTES),
            "outcome": crate::report::word(outcome), "exit_code": run.code,
            "waiting_s": ran.waiting.as_secs_f64(), "execution_s": ran.execution.as_secs_f64(),
            "cpu_s": usage.map(|u| u.cpu.as_secs_f64()),
            "starved_s": usage.map(|u| u.starved.as_secs_f64()),
            "peak_bytes": usage.map(|u| u.peak_bytes),
            "width": width, "charge": ran.planned.charge, "tests": tests,
            "test_counts": test_counts,
            "failures": failures,
            "failures_complete": false,
            "failure_observation_end": run.case_observations.as_ref().map(|o| o.read_end.name()),
            "detail": if run.ok {
                String::new()
            } else if let Some(hung) = &run.hung {
                crate::report::clip_json(&hung.detail(), crate::report::EVENT_DETAIL_BYTES)
            } else if let Some(stopped) = &run.stopped_at {
                stopped_detail(stopped, combined.as_ref().map_or("", |c| c.output.as_str()))
            } else {
                unavailable.unwrap_or_else(|| first_error(&run.output))
            },
            "stopped_at": run.stopped_at.as_ref()
                .map(|s| crate::report::clip_json(&s.case, crate::report::EVENT_ROW_BYTES)),
            "hung": run.hung.is_some(),
            "hung_binary": hung_binary,
            "hung_cases": hung_cases,
            "hung_cases_omitted": hung_cases_omitted,
            "hang_window_s": run.hung.as_ref().map(|h| h.window.as_secs_f64()),
            "stack_sample": stack_sample,
            "stack_sample_error": stack_sample_error,
            "evidence": evidence.map(|evidence| aterm_json::json!({
                "schema": crate::test_content::SCHEMA,
                "exe_sha256": evidence.exe_sha256,
                "content_key": evidence.content_key,
                "inputs_bounded": evidence.bounded(),
                "input_reason": evidence.reason,
            })),
            "exe_sha256": evidence.and_then(|evidence| evidence.exe_sha256.as_deref()),
            "content_key": evidence.and_then(|evidence| evidence.content_key.as_deref()),
            "inputs_bounded": evidence.map(Evidence::bounded),
            "input_reason": evidence.and_then(|evidence| evidence.reason.as_deref()),
            "json": json,
            "recorded_execution_s": ran.execution.as_secs_f64(),
            "carried": false,
        })
    );
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let stopped =
        started && (run.stopped_at.is_some() || run.hung.is_some()) && run.spawn_error.is_none();
    let sample = (measured(run, started) || stopped).then(|| crate::test_history::Sample {
        at_ms: aterm_time::unix_millis().unwrap_or(0),
        outcome: crate::report::word(outcome).to_owned(),
        wall_ms: if stopped { 0 } else { ms(ran.execution) },
        cpu_ms: if stopped {
            None
        } else {
            usage.map(|u| ms(u.cpu))
        },
        starved_ms: if stopped {
            None
        } else {
            usage.map(|u| ms(u.starved))
        },
        peak_bytes: if stopped {
            None
        } else {
            usage.map(|u| u.peak_bytes)
        },
        width,
        tests,
        partition: learned_partition,
    });
    (outcome, event, sample)
}

/// Classify a binary's outcome before its event is written. A decisive red stops the local
/// dispatcher through `halt`, where one is still running; that halt takes the admission mutex, so
/// once it returns no queued binary can start. The caller writes its FAIL event before arming the
/// run-wide halt, so the event still precedes the NOT RUN events of peers that halt ends.
fn mark_red_decided(outcome: Outcome, stops_at_red: bool, halt: Option<&dyn Fn()>) -> bool {
    let red = stops_at_red && outcome == Outcome::Fail(Severity::GateFailed);
    if red && let Some(halt) = halt {
        halt();
    }
    red
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
/// holder it waited behind, a wait for it that failed, or an end unrun at macOS's program check,
/// `exec::start`: a program its syspolicyd stranded, or at its ceiling a leader not yet started or
/// a member held past a first exec's bound) and no interrupt was ending the run as it ended
/// ([`crate::exec::ending`]), which cuts a binary short at a point that says nothing of it. A
/// binary the run's ceiling ended keeps its sample, a lower bound of what it takes, unless verify
/// ended it unrun there; one that never started would keep a wall of about 0 ms, and push a
/// measured sample out of its row. A binary verify stopped at its first red case
/// ([`Run::stopped_at`]) is deliberately not measured: its caller keeps an outcome-only schema-1
/// sample with a zero wall, so the latest FAIL orders it first without replacing its last timed
/// wall.
fn measured(run: &Run, started: bool) -> bool {
    started
        && run.stopped_at.is_none()
        && run.hung.is_none()
        && run.spawn_error.is_none()
        && !crate::exec::ending()
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
    let resources = crate::current_resource_claim();
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, order.len().max(1)) {
            scope.spawn(|| {
                let _metering = crate::memory::metering(meter.clone());
                let _covering = crate::memory::covering(cover.clone());
                let _reservation = crate::holding_resource_claim(resources.clone());
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
        .filter(|(k, _)| {
            k != "RUST_TEST_THREADS"
                && k != crate::stages::CHECKERS_ENV
                && !k.as_encoded_bytes().starts_with(b"ATERM_RELEASE_")
        })
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
    if let Some(seed) = watch.seed.filter(|_| b.harness) {
        if !json {
            command.args.push("-Zunstable-options".into());
        }
        command.args.push(format!("--shuffle-seed={seed}").into());
    }
    command.test_cases = Some(exec::CaseWatch {
        spec: b.spec.clone(),
        binary: b.record.exe().display().to_string(),
        json,
        stop_at_red: watch.stop_at_red,
        hang_after: exec::TEST_BINARY_HANG,
    });
    crate::stages::with_checkers(command, checkers)
}

/// The exact environment and invocation boundary a test binary crosses. Evidence and
/// execution both use this constructor; content identity normalizes only the scheduler-controlled
/// values which `exec` may lower after this [`Cmd`] is built.
fn final_binary_cmd(
    ctx: &crate::Ctx,
    binary: &Binary,
    threads: u32,
    checkers: bool,
    watch: Watch,
) -> Cmd {
    crate::test_env::command(ctx, binary_cmd(binary, threads, checkers, watch))
}

/// The one log cargo would have written for these runs, in declared order, except that a run with
/// a `spawn_error` (something outside the binary ended it: [`split_unrun`]) adds its own output
/// alone: it is no failed target, and the returned `ok` says nothing of it. Its own row says why
/// ([`decide_runs`]).
#[must_use]
pub fn combined(bins: &[Binary], runs: &[Run]) -> Run {
    let mut output = String::new();
    let mut failed = Vec::new();
    let hung = runs.iter().find_map(|run| run.hung.clone());
    for (b, r) in bins.iter().zip(runs) {
        output.push_str(&r.output);
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if r.ok || r.spawn_error.is_some() {
            continue;
        }
        output.push_str(&format!("error: test failed, to rerun pass `{}`\n", b.spec));
        if let Some(hung) = &r.hung {
            output.push_str(&format!(
                "note: verify ended this binary as {}\n",
                hung.detail()
            ));
        } else if let Some(stopped) = &r.stopped_at {
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
        case_observations: None,
        stopped_at: None,
        hung,
        ok: failed.is_empty(),
        output,
        code: Some(if failed.is_empty() { 0 } else { 101 }),
        spawn_error: None,
        usage: None,
    }
}

/// The ladder's one line about how the binaries ran: on what share, the most that ran at once,
/// their widths, and how many had no runs kept to plan by.
#[must_use]
pub fn summary(plan: &Plan, ran: &[Option<Ran>]) -> String {
    let n = plan.order.len();
    let widths = plan.order.iter().map(|b| b.width);
    let (lo, hi) = (widths.clone().min().unwrap_or(0), widths.max().unwrap_or(0));
    let widths = if lo == hi {
        format!("RUST_TEST_THREADS={lo} each")
    } else {
        format!("RUST_TEST_THREADS={lo} to {hi}")
    };
    let unseen = plan.order.iter().filter(|b| b.predicted.is_none()).count();
    let unseen = match unseen {
        0 => String::new(),
        k if k == n => "; none had runs kept to plan by".to_owned(),
        k => format!("; {k} had no runs kept to plan by"),
    };
    format!(
        "  test binaries: {n} on a share of {} cores — {widths}, at most {} at once{unseen}",
        plan.share,
        most_at_once(ran)
    )
}

/// The most binaries that ran at once: the most `[waiting, waiting + execution]` spans that
/// overlap. A binary the run left unstarted ran for no time and holds no span.
fn most_at_once(ran: &[Option<Ran>]) -> usize {
    let mut edges: Vec<(Duration, bool)> = ran
        .iter()
        .flatten()
        .filter(|r| !r.execution.is_zero())
        .flat_map(|r| {
            [
                (r.waiting, true),
                (r.waiting.saturating_add(r.execution), false),
            ]
        })
        .collect();
    // An end before a start at the same instant: one that ended as the next began ran alone.
    edges.sort_by_key(|&(at, start)| (at, start));
    let (mut now, mut most) = (0_usize, 0_usize);
    for (_, start) in edges {
        if start {
            now += 1;
            most = most.max(now);
        } else {
            now = now.saturating_sub(1);
        }
    }
    most
}

/// The ladder's line naming the binaries that used more cores than they were charged, with what
/// each used: each is charged that much next run. Empty where none did.
#[must_use]
pub fn overruns(bins: &[Binary], ran: &[Option<Ran>]) -> String {
    let over: Vec<String> = bins
        .iter()
        .zip(ran)
        .filter_map(|(b, r)| {
            let r = r.as_ref()?;
            let used = r.overran()?;
            Some(format!(
                "{} used {used:.1} cores of {} charged",
                b.spec, r.planned.charge
            ))
        })
        .collect();
    if over.is_empty() {
        return String::new();
    }
    format!(
        "  test binaries over their charge (charged what they used next run): {}",
        over.join(", ")
    )
}

/// A binary the test run's ceiling left no time to start.
#[must_use]
pub fn not_started(b: &Binary, limit: Duration) -> Run {
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
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
fn decide(
    ctx: &crate::Ctx,
    r: &mut Report,
    run: &Run,
    label: &str,
    rerun: &crate::findings::Rerun,
) {
    if ctx.checkers_in_tests && crate::stages::compared_no_frame(run) {
        r.cannot_run(format!(
            "{label} — {} did not run, so no frame was held to its twin",
            crate::checks::GOLDEN.1
        ));
        return;
    }
    r.decide_test_child(run, label, rerun);
}

/// The binaries whose runs carry a `spawn_error`, each with it, apart from the rest with their
/// runs: something outside the binary ended it (a spawn refused, an interrupt, a stuck holder it
/// waited behind, a wait for it that failed, or verify's end of it unrun at macOS's program check,
/// `exec::start`: a program its syspolicyd stranded, or at its ceiling a leader not yet started
/// or a member held past a first exec's bound), so its run decided nothing about the tree.
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

/// Hold `claim` on this thread until the answer drops: its reservation retained, and its cover
/// marker handed to each child the thread starts ([`crate::memory::cover`]); nothing more with no
/// claim.
fn holding(
    claim: Option<&crate::ResourceClaim>,
) -> (crate::ReservationScope, Option<crate::memory::Covering>) {
    (
        crate::holding_resource_claim(claim.map(|c| std::sync::Arc::clone(&c.claim))),
        claim.map(|c| crate::memory::covering(c.cover())),
    )
}

/// The ladder's line saying what `claim` claimed for the test binaries; none with no claim.
fn claimed_said(r: &mut Report, claim: Option<&crate::ResourceClaim>) {
    if let Some(claim) = claim {
        r.raw(format!("  test binaries' {}", claim.said));
    }
}

/// A test run's claims in its capacity ledger: memory for its binaries and cores for their
/// subprocesses ([`claim_both`]).
pub(crate) struct Claims {
    /// The memory claim; none for nothing to claim, or a run with no book.
    pub(crate) resources: Option<crate::ResourceClaim>,
    /// The cores claim, held until the claims go back; none in a run with no book.
    _cores: Option<aterm_capacity::Claim>,
    /// The cores granted: at least one, at most those asked.
    pub(crate) granted: u32,
}

/// Claim `bytes` of memory for `label`'s test binaries (`what` says for what), then `asked`
/// cores, each admitted by `admit`, once the run's claims before (`held`) have gone back: memory
/// before cores, the order every claim of both is made in (a stage's memory, then its
/// subprocesses' cores, or both in one claim), so a test run never waits for memory while it
/// holds cores, and never waits on a run that holds memory and waits for cores.
///
/// # Errors
/// The NOT RUN row's words: why the resources were not claimed
/// ([`crate::claim_test_resources`]), or the cores ([`claim_cores`]).
pub(crate) fn claim_both(
    ctx: &crate::Ctx,
    label: &str,
    held: Option<Claims>,
    bytes: u64,
    what: &str,
    asked: u32,
    admit: crate::Admitter,
) -> Result<Claims, String> {
    drop(held);
    if ctx.background && ctx.pool.is_none() {
        let resources = crate::claim_test_resources(ctx, label, bytes, what, asked, admit)
            .map_err(|why| format!("{label} — {why}"))?;
        let granted = resources
            .as_ref()
            .map_or(asked, |claim| claim.claim.cores().min(asked));
        if granted == 0 {
            return Err("subprocess CPU admission granted no cores; no test was run".to_owned());
        }
        return Ok(Claims {
            resources,
            _cores: None,
            granted,
        });
    }
    let resources = crate::claim_test_resources(ctx, label, bytes, what, 0, admit)
        .map_err(|why| format!("{label} — {why}"))?;
    let (cores, granted) = claim_cores(ctx, label, asked, admit)?;
    Ok(Claims {
        resources,
        _cores: cores,
        granted,
    })
}

/// Claim `asked` cores for `label`'s test binaries in the run's capacity ledger, waiting for them
/// (`admit`): the claim, none in a run with no book, and the cores it granted, at least one and
/// at most `asked`.
///
/// # Errors
/// The NOT RUN row's words: the halt's, where it ended the wait; a refused admission; or a
/// grant of no core.
fn claim_cores(
    ctx: &crate::Ctx,
    label: &str,
    asked: u32,
    admit: crate::Admitter,
) -> Result<(Option<aterm_capacity::Claim>, u32), String> {
    // Under the release's pool the binaries' charges are its tokens ([`dispatch_pooled`]), which
    // the release's own Release core claim bounds: a claim here would count its other branches'
    // claims (the build's, the export's) and wait for them to end.
    if ctx.memory.is_none() || ctx.pool.is_some() {
        return Ok((None, asked));
    }
    match admit(
        aterm_capacity::Need::new(format!("verify {label} subprocesses")).cores(asked),
        aterm_capacity::Admit::Wait,
    ) {
        Ok(claim) => match claim.cores() {
            0 => Err("subprocess CPU admission granted no cores; no test was run".to_owned()),
            cores => Ok((Some(claim), cores.min(asked))),
        },
        Err(aterm_capacity::Error::Stopped { .. }) if ctx.halted().is_some() => Err(format!(
            "{label} — {}; no test was run",
            ctx.halted().unwrap_or_default()
        )),
        Err(why) => Err(format!(
            "subprocess CPU admission refused: {why}; no test was run"
        )),
    }
}

/// Record Cargo's current test invocations and run them on the run's CPU share: each binary at
/// the width its kept runs call for and charged the cores they used, as many at once as their
/// charges fit in the share, the binaries that failed last first ([`crate::test_plan`]); and keep
/// what each took for the next run ([`crate::test_history`]). Each binary that runs libtest's
/// harness runs in libtest's JSON format where libtest takes it ([`probe_json`]), so each failed
/// case's block is read as the case ends. The first proven red ends the run when its halt is armed
/// ([`crate::Ctx::halt`], named for `check`): a decisive failed case ends its binary there
/// ([`crate::libtest_json::render`]), while a case that refused its machine or hit a full disk
/// ends nothing. A human-format binary is decided at its end. No binary starts after the red,
/// and those it stopped are one NOT RUN row with the halt's words. A binary verify could not
/// start or ended unrun otherwise is a NOT RUN row of its own, and the binaries that ran decide
/// the run's row ([`decide_runs`]).
pub fn run_tests(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    serial: &Cmd,
    check: crate::checks::Check,
) {
    let mut progress = Progress::new(ctx.events.as_ref(), check);
    progress.enter(Phase::Preparation);
    if let Err(why) = crate::test_env::prepare(ctx) {
        r.cannot_run(format!(
            "cannot prepare release test environment: {why}; no test was run"
        ));
        return;
    }
    if let Err(why) = crate::test_workers::share(&ctx.env, ctx.pool.as_ref()) {
        r.cannot_run(format!("{why}; no test was run"));
        return;
    }
    let asked_cores = ctx.test_share();
    let history = ctx
        .test_history
        .as_deref()
        .map(crate::test_history::load)
        .unwrap_or_default();
    // The exact-host recording pass compiles what Cargo still finds stale and runs only the small
    // recorder process in place of each test binary. Conservatively reserve the selection's
    // heaviest kept peak, or its cold peak, then the run's cores ([`claim_both`]); both are held
    // until the recorded binaries' plan replaces them.
    let fallback_peak = fallback_peak(
        &history,
        ctx.scope.crates(),
        ctx.memory.as_ref().and_then(|book| book.physical),
    );
    progress.enter(Phase::RecordingAdmission);
    let claims = match claim_both(
        ctx,
        label,
        None,
        aterm_capacity::peak_claim(fallback_peak),
        "for the exact-host recording pass, at the selection's kept or cold peak",
        asked_cores,
        crate::admit_live,
    ) {
        Ok(claims) => claims,
        Err(why) => {
            r.cannot_run(why);
            return;
        }
    };
    progress.enter(Phase::RecordingSetup);
    let granted_cores = claims.granted;
    let recording_resources = claims.resources.as_ref();
    let thread_override = (ctx.env.build_slot_held.is_some()
        || ctx.pool.is_some()
        || ctx.test_threads.is_some()
        || ctx.test_cores.is_some()
        || granted_cores < asked_cores)
        .then(|| ctx.test_threads_within(granted_cores));
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
    serial = crate::test_env::command(ctx, crate::stages::with_checkers(serial, checkers))
        .env(SUBPROCESS_SLOTS_ENV, slots.path());
    let serial = &serial;
    match configured_build_targets(ctx, serial) {
        Ok(None) => {}
        Ok(Some(targets)) => {
            r.cannot_run(format!(
                "Cargo selects build.target={targets:?}; the native test dispatcher will not \
                 enter explicit-target mode, so no test binary was run"
            ));
            return;
        }
        Err(why) => {
            r.cannot_run(format!("{why}; no test binary was run"));
            return;
        }
    }
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
    let rerun = crate::findings::Rerun::of(serial, thread_override).with_seed(ctx.test_seed);
    let dir = ctx.scratch.join(format!(
        "test-records.{}.{}",
        std::process::id(),
        RECORDING.fetch_add(1, Ordering::SeqCst)
    ));
    let gate = ctx.test_recorder();
    let host = match crate::cells::host_triple(ctx) {
        Ok(host) => host,
        Err(why) => {
            r.cannot_run(format!("cannot select the test recording runner: {why}"));
            return;
        }
    };
    let config = match (&gate, std::fs::create_dir_all(&dir)) {
        (Some(gate), Ok(())) => runner_config(gate, &dir, &host),
        _ => None,
    };
    let Some(config) = config else {
        std::fs::remove_dir_all(&dir).ok();
        r.cannot_run("verify could not name itself as the exact-host test recording runner");
        return;
    };
    progress.enter(Phase::Recording);
    let began = Instant::now();
    let mut pass = {
        let _held = holding(recording_resources);
        exec::run(&recording_cmd(serial, &config, &host), ctx.exec_env())
    };
    let pass_wall = began.elapsed();
    progress.enter(Phase::Planning);
    let records = read_records(&dir);
    std::fs::remove_dir_all(&dir).ok();

    if let Err(why) = &records {
        r.cannot_run(format!("the test recording could not be read: {why}"));
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
        r.cannot_run(
            "cargo announced test binaries, but the exact-host recording runner left no record; \
             no test binary was run",
        );
        return;
    }
    if !recorded && !recorder_failed {
        // No test binary was built, so no recorder invocation was expected.
        if !announced(&pass.output).is_empty() {
            r.cannot_run("cargo announced a test binary but the recording runner did not run");
            return;
        }
        r.raw(pass.output.as_str());
        decide(ctx, r, &pass, label, &rerun);
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
    crate::event_log::emit(ctx.events.as_ref(), &account.event());
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
            r.cannot_run(format!("{why}; no test binary was run"));
            return;
        }
    };
    let preface: String = pass
        .output
        .lines()
        .filter(|l| !l.trim_start().starts_with("Running "))
        .map(|l| format!("{l}\n"))
        .collect();
    let planned = RunPlan {
        bins,
        history,
        fallback_peak,
        claims,
        asked_cores,
        granted_cores,
        checkers,
        preface,
        rerun: rerun.clone(),
    };
    run_planned_observed(
        ctx,
        r,
        label,
        planned,
        check,
        exec::TEST_BINARY_HANG,
        &mut progress,
    );
}

/// What [`run_tests`] planned for its binaries' runs.
pub(crate) struct RunPlan {
    /// The binaries, in declared order.
    pub(crate) bins: Vec<Binary>,
    /// The history that sizes and orders them.
    pub(crate) history: crate::test_history::History,
    /// The selection's heaviest kept peak, or its cold fallback, used where a binary has no peak.
    pub(crate) fallback_peak: u64,
    /// The recording pass's memory and core claims, replaced by the dispatch's claims.
    pub(crate) claims: Claims,
    /// The CPU share the run asked for.
    pub(crate) asked_cores: u32,
    /// The share the recording pass was granted, used for the full binary set's initial plan.
    pub(crate) granted_cores: u32,
    /// The run's checker policy ([`crate::stages::with_checkers`]).
    pub(crate) checkers: bool,
    /// The recording pass's log, its `Running` headers dropped, at the head of a whole run's
    /// report.
    pub(crate) preface: String,
    /// The environment a failed test reruns under.
    pub(crate) rerun: crate::findings::Rerun,
}

#[cfg(test)]
impl RunPlan {
    /// A test-only run over `bins` on `share`, with no capacity book or kept history.
    pub(crate) fn fixture(bins: Vec<Binary>, share: u32) -> RunPlan {
        let share = share.max(1);
        RunPlan {
            bins,
            history: crate::test_history::History::default(),
            fallback_peak: aterm_capacity::cold_claim(None),
            claims: Claims {
                resources: None,
                _cores: None,
                granted: share,
            },
            asked_cores: share,
            granted_cores: share,
            checkers: false,
            preface: String::new(),
            rerun: crate::findings::Rerun::default(),
        }
    }
}

/// The raw peak used where this selection has no measured test-binary peak: its heaviest kept
/// peak, else the machine's cold figure. It is positive even if a corrupt machine reading said
/// it had fewer than two bytes.
fn fallback_peak(
    history: &crate::test_history::History,
    crates: &[String],
    physical: Option<u64>,
) -> u64 {
    match history.heaviest(crates) {
        0 => aterm_capacity::cold_claim(physical).max(1),
        peak => peak,
    }
}

/// Plan `bins` from their history, replacing every missing peak (an unseen binary or a timed
/// sample whose tree usage was unread) with `fallback_peak`. Thus every dispatched binary has a
/// positive memory cost until this machine has measured one for it.
fn plan_from_history(
    history: &crate::test_history::History,
    bins: &[Binary],
    share: u32,
    ceiling: Option<u32>,
    fallback_peak: u64,
) -> Plan {
    let seen: Vec<_> = bins
        .iter()
        .map(|b| {
            crate::test_shards::seen(history, &b.spec).map(|mut seen| {
                if seen.peak == 0 {
                    seen.peak = fallback_peak;
                }
                seen
            })
        })
        .collect();
    test_plan::plan(&seen, share, ceiling, fallback_peak)
}

/// Remove one binary from a fixed plan after its JSON probe itself ended hung. Every peer keeps
/// the width, charge, peak, prediction and relative dispatch order the full-set claim was based
/// on; only cargo-order indexes above the removed binary move down one.
fn remove_plan_index(plan: &mut Plan, removed: usize) {
    plan.order.retain(|planned| planned.index != removed);
    for planned in &mut plan.order {
        if planned.index > removed {
            planned.index -= 1;
        }
    }
}

/// Project a fixed full-set plan onto `kept` binaries, identified by their unique Cargo rerun
/// specs. This is what carry uses: removing an equal-content binary cannot replan a miss more
/// widely and thereby change the command whose content key was just judged.
fn project_plan(plan: &Plan, full: &[Binary], kept: &[Binary]) -> Plan {
    let positions: BTreeMap<_, _> = kept
        .iter()
        .enumerate()
        .map(|(index, binary)| (binary.spec.as_str(), index))
        .collect();
    let order: Vec<_> = plan
        .order
        .iter()
        .filter_map(|planned| {
            let spec = full.get(planned.index)?.spec.as_str();
            positions
                .get(spec)
                .map(|&index| Planned { index, ..*planned })
        })
        .collect();
    assert_eq!(
        order.len(),
        kept.len(),
        "fixed test plan and carried binary set disagree"
    );
    Plan {
        share: plan.share,
        target: plan.target,
        order,
    }
}

/// The memory claim for the binaries `plan` can run at once.
fn plan_claim(plan: &Plan) -> Option<u64> {
    let peak = test_plan::schedule(plan).peak;
    (peak > 0).then(|| aterm_capacity::peak_claim(peak))
}

/// What [`probe_json`] learned of libtest's JSON format.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probe {
    /// A binary listed its cases in it.
    Took,
    /// A binary ran and exited without it: what libtest said, its first line.
    Refused(String),
    /// No binary it asked answered (a spawn refused, an end unrun at macOS's program check, a
    /// crash, a run already ending): why the last one did not.
    Unanswered(String),
}

struct ProbeHung {
    index: usize,
    run: Box<Run>,
    elapsed: Duration,
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
const JSON_FLAGS: [&str; 5] = [
    "-Z",
    "unstable-options",
    "--format",
    "json",
    "--report-time",
];

/// The binaries [`probe_json`] asks before it takes no answer for one.
const PROBE_TRIES: usize = 3;

/// Ask the binaries that run libtest's harness ([`Binary::harness`]), in order, to list their
/// cases in libtest's JSON format until one answers, at most [`PROBE_TRIES`] of them: `None`
/// where no binary runs the harness. A binary that exited with a code answers: [`Probe::Took`]
/// where it exited 0 and printed a suite `discovery` event, else [`Probe::Refused`] with its first
/// line, what libtest said in refusing. One that verify could not start or ended
/// ([`Run::spawn_error`]), that a signal ended, or that ended as the run was ending says nothing
/// of libtest, so the next is asked; with none left, [`Probe::Unanswered`].
fn probe_json(
    ctx: &crate::Ctx,
    bins: &[Binary],
    hang_after: Duration,
) -> Result<Option<Probe>, ProbeHung> {
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
    for (index, b) in bins
        .iter()
        .enumerate()
        .filter(|(_, b)| b.harness)
        .take(PROBE_TRIES)
    {
        let mut argv = vec![b.record.exe().as_os_str().to_owned()];
        argv.extend(JSON_FLAGS.iter().map(OsString::from));
        argv.push("--list".into());
        let mut cmd = Cmd::replayed(&argv, &b.record.cwd, b.record.env.clone());
        cmd.capture = exec::Capture::Emit;
        cmd.test_cases = Some(exec::CaseWatch {
            spec: b.spec.clone(),
            binary: b.record.exe().display().to_string(),
            json: true,
            stop_at_red: false,
            hang_after,
        });
        let begun = Instant::now();
        let run = exec::run(&cmd, ctx.exec_env());
        let elapsed = begun.elapsed();
        if run.hung.is_some() {
            return Err(ProbeHung {
                index,
                run: Box::new(run),
                elapsed,
            });
        }
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
        return Ok(Some(if run.ok && discovered {
            Probe::Took
        } else {
            Probe::Refused(first_line(&run))
        }));
    }
    Ok(unanswered.map(Probe::Unanswered))
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
    /// Explicit deterministic shuffle, only for libtest binaries.
    pub seed: Option<u64>,
}

/// Run the planned binaries: each through [`pool`], its cases read as they end ([`Watch`]), and
/// decide the run's rows ([`decide_runs`]). The first binary that fails ends the run when its halt
/// is armed: a JSON run ends that binary at its first decisive red case (`exec`'s `LiveCases`), a
/// human one at its end; no binary starts after it, and those it stopped are one NOT RUN row with
/// the halt's words.
#[cfg(test)]
pub(crate) fn run_planned(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    planned: RunPlan,
    check: crate::checks::Check,
) {
    run_planned_with_probe_window(ctx, r, label, planned, check, exec::TEST_BINARY_HANG);
}

/// [`run_planned`], with the JSON probe's no-progress window injected for deterministic tests.
#[cfg(test)]
fn run_planned_with_probe_window(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    planned: RunPlan,
    check: crate::checks::Check,
    probe_hang_after: Duration,
) {
    let mut progress = Progress::new(ctx.events.as_ref(), check);
    run_planned_observed(
        ctx,
        r,
        label,
        planned,
        check,
        probe_hang_after,
        &mut progress,
    );
}

fn run_planned_observed(
    ctx: &crate::Ctx,
    r: &mut Report,
    label: &str,
    planned: RunPlan,
    check: crate::checks::Check,
    probe_hang_after: Duration,
    progress: &mut Progress<'_>,
) {
    progress.enter(Phase::Planning);
    let RunPlan {
        mut bins,
        history,
        fallback_peak,
        claims,
        asked_cores,
        granted_cores,
        checkers,
        preface,
        rerun,
    } = planned;
    bins = crate::test_shards::expand(bins, &history, ctx.passed_binaries.as_ref());
    let listed = bins.len();
    // Plan and claim the complete recorded set. A carry may remove work, but cannot change any
    // peer's width or charge after that peer's content key has committed to its command.
    let plan = plan_from_history(
        &history,
        &bins,
        granted_cores,
        ctx.test_threads,
        fallback_peak,
    );
    let claimed = plan_claim(&plan);
    // The full plan's claims replace the recording claims, memory before cores as ever. A moved
    // grant may change the full plan once, before probing or content evidence exists.
    progress.enter(Phase::ExecutionAdmission);
    let claims = match claim_both(
        ctx,
        label,
        Some(claims),
        claimed.unwrap_or(0),
        "for the test binaries its plan runs at once, by their runs",
        asked_cores,
        crate::admit_live,
    ) {
        Ok(claims) => claims,
        Err(why) => {
            r.cannot_run(why);
            return;
        }
    };
    progress.enter(Phase::Planning);
    let mut plan = if claims.granted == plan.share {
        plan
    } else {
        plan_from_history(
            &history,
            &bins,
            claims.granted,
            ctx.test_threads,
            fallback_peak,
        )
    };
    // A failed test reruns at the width its binary runs at, which the full plan sets. A probe hang
    // or carry only projects this plan; neither replans the peers after their widths are fixed.
    let rerun_base = rerun;
    let claim = claims.resources.as_ref();
    // The binaries' peaks are kept within the claim, and a claim past what can ever be available
    // here, which runs alone, within what can.
    let memory = claim
        .and(claimed)
        .map(|c| ctx.memory.as_ref().map_or(c, |book| c.min(book.budget())));
    let _held = holding(claim);
    // A candidate is judged before any harness process starts. Reconstruct the prior run's
    // global JSON decision, finalize that exact command under this hop's already-fixed width and
    // held claim, and carry only an equal complete key. The probe below sees misses alone, so a
    // binary reported as carried has not even run startup code in this hop.
    let evidenced = AtomicU64::new(0);
    let verifier = crate::report::running_driver().map(str::to_owned);
    let new_evidence_builder = || {
        crate::test_content::EvidenceBuilder::new(
            ctx.carry_context.clone(),
            verifier.clone(),
            ctx.scratch.clone(),
        )
    };
    let candidate_evidence = new_evidence_builder();
    let widths: BTreeMap<_, _> = plan
        .order
        .iter()
        .map(|planned| (bins[planned.index].spec.clone(), planned.width))
        .collect();
    let command_for = |binary: &Binary, json: bool| {
        let width = widths
            .get(&binary.spec)
            .copied()
            .expect("every release binary is in the fixed test plan");
        let command = final_binary_cmd(
            ctx,
            binary,
            width,
            checkers,
            Watch {
                json,
                stop_at_red: ctx.halt.armed(),
                seed: ctx.test_seed,
            },
        );
        match exec::finalize_exact_command(command.clone(), ctx.exec_env()) {
            Ok(finalized) => (finalized, None),
            Err(why) => (command, Some(why)),
        }
    };
    let evidence_for = |builder: &crate::test_content::EvidenceBuilder,
                        binary: &Binary,
                        command: &Cmd,
                        exact_error: Option<&str>| {
        let began = Instant::now();
        let mut evidence = if exact_error.is_some() {
            let mut incomplete = command.clone();
            incomplete.exact_env = false;
            builder.evidence(binary, &incomplete)
        } else {
            builder.evidence(binary, command)
        };
        if let Some(why) = exact_error {
            evidence.content_key = None;
            evidence.reason = Some(format!("the exact spawn environment is unbounded: {why}"));
        }
        evidenced.fetch_add(
            u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        evidence
    };
    let candidate_for = |binary: &Binary, json: bool| {
        let (command, exact_error) = command_for(binary, json);
        evidence_for(
            &candidate_evidence,
            binary,
            &command,
            exact_error.as_deref(),
        )
    };
    let empty_passed = Passed::new();
    let (carried, known) = if ctx.release {
        let all = bins;
        let Carried {
            run,
            carried,
            mut known,
        } = carry(
            ctx.passed_binaries.as_ref().unwrap_or(&empty_passed),
            all.clone(),
            |binary, prior| {
                prior.map_or_else(
                    || Evidence {
                        exe_sha256: None,
                        content_key: None,
                        reason: Some("there is no prior successful execution to carry".into()),
                    },
                    |proof| candidate_for(binary, proof.json),
                )
            },
            usize::try_from(claims.granted).unwrap_or(1),
        );
        plan = project_plan(&plan, &all, &run);
        for binary in &run {
            known.remove(&binary.spec);
        }
        bins = run;
        (carried, known)
    } else {
        (Vec::new(), BTreeMap::new())
    };
    let evidence = Mutex::new(known);
    let evidence_of = |b: &Binary| {
        evidence
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&b.spec)
            .cloned()
    };
    // Carry selection happens before a harness starts, but another miss may mutate a candidate's
    // declared input later. Delay every carried OK until all probe/dispatch activity ends, then
    // rehash with a fresh builder. Persistent drift becomes a final NOT RUN row and an incomplete
    // check; `read-only = true` is the run-wide audit boundary for transient restore writes.
    let finish_carried = || {
        let mut drifted = Vec::new();
        for candidate in &carried {
            let (command, exact_error) = command_for(&candidate.binary, candidate.json);
            let after = evidence_for(
                &new_evidence_builder(),
                &candidate.binary,
                &command,
                exact_error.as_deref(),
            );
            if after == candidate.evidence {
                evidence
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(candidate.binary.spec.clone(), after);
                emit(ctx, &carried_event(candidate));
            } else {
                let changed = Evidence {
                    exe_sha256: after
                        .exe_sha256
                        .or_else(|| candidate.evidence.exe_sha256.clone()),
                    content_key: None,
                    reason: Some(
                        "runtime inputs changed after carry selection; the run-wide read-only \
                         declaration was violated"
                            .into(),
                    ),
                };
                evidence
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(candidate.binary.spec.clone(), changed.clone());
                emit(ctx, &unstable_carried_event(candidate, &changed));
                drifted.push(candidate.binary.spec.clone());
            }
        }
        drifted
    };
    let mut preface = preface;
    let mut probe_hung_count = 0_usize;
    let mut probe_samples = Vec::new();
    progress.enter(Phase::Probing);
    let probe = loop {
        match probe_json(ctx, &bins, probe_hang_after) {
            Ok(probe) => break probe,
            Err(hung) => {
                let ProbeHung {
                    index,
                    run,
                    elapsed,
                } = hung;
                let planned = plan
                    .order
                    .iter()
                    .find(|planned| planned.index == index)
                    .copied()
                    .expect("every probed binary is in the test plan");
                let binary = bins.remove(index);
                let ran = Ran {
                    run: *run,
                    planned,
                    waiting: Duration::ZERO,
                    execution: elapsed,
                };
                let (outcome, event, sample) = binary_event(&binary, &ran, true, None, true);
                let red = mark_red_decided(outcome, ctx.halt.armed(), None);
                emit(ctx, &event);
                if red {
                    ctx.halt.red(check.name());
                }
                if let Some(sample) = sample {
                    probe_samples.push((binary.spec.clone(), sample));
                }
                r.raw(std::mem::take(&mut preface));
                r.raw("  test cases: the libtest JSON probe stopped at a hung binary");
                r.raw(ran.run.output.as_str());
                let hung_rerun = rerun_base
                    .clone()
                    .at_widths([(binary.spec.clone(), planned.width)]);
                decide_runs(
                    r,
                    std::slice::from_ref(&binary),
                    std::slice::from_ref(&ran.run),
                    label,
                    |r, run| decide(ctx, r, run, label, &hung_rerun),
                );
                probe_hung_count += 1;
                remove_plan_index(&mut plan, index);
                if bins.is_empty() {
                    if let Some(Err(why)) = ctx
                        .test_history
                        .as_deref()
                        .map(|path| crate::test_history::keep(path, probe_samples))
                    {
                        r.raw(format!(
                            "  test binaries' history not kept (planning data only): {why}"
                        ));
                    }
                    let drifted = finish_carried();
                    claimed_said(r, claim);
                    if !drifted.is_empty() {
                        r.cannot_run(format!(
                            "{label} — {} carried candidate(s) changed after selection and were \
                             not run: {}",
                            drifted.len(),
                            drifted.join(", ")
                        ));
                    }
                    return;
                }
                if ctx.halt.armed() {
                    break None;
                }
                // A halt-off sweep still owes every peer a run. Probe the remaining harnesses
                // again: no hung binary is retried, and a later hang still has JSON case names.
            }
        }
    };
    let rerun = rerun_base.at_widths(
        plan.order
            .iter()
            .map(|planned| (bins[planned.index].spec.clone(), planned.width)),
    );
    if let Some(probe) = &probe {
        r.raw(json_ladder(probe));
    }
    let watch = Watch {
        json: probe == Some(Probe::Took),
        stop_at_red: ctx.halt.armed(),
        seed: ctx.test_seed,
    };
    // Misses now have the probe's actual mode. Finalize and retain their exact commands, but do
    // not hash runtime inputs yet: an earlier admitted binary may legitimately precede this one.
    // Each miss is hashed immediately before its own spawn below.
    let commands = if ctx.release {
        let asked: Vec<usize> = (0..bins.len()).collect();
        let completed = pool(
            bins.len(),
            &asked,
            usize::try_from(claims.granted).unwrap_or(1),
            |index| {
                let binary = &bins[index];
                let (command, exact_error) = command_for(binary, watch.json);
                (binary.spec.clone(), command, exact_error)
            },
        );
        let mut commands = BTreeMap::new();
        for completed in completed.into_iter().flatten() {
            let (spec, command, exact_error) = completed;
            commands.insert(spec, (command, exact_error));
        }
        commands
    } else {
        BTreeMap::new()
    };
    let meter = crate::memory::current();
    let live = || meter.as_ref().and_then(|m| m.live_rate()).unwrap_or(0.0);
    let ceiling = ctx.exec_env().child_ceiling;
    progress.enter(Phase::Dispatch);
    let started = Instant::now();
    let spent = || ceiling.is_some_and(|limit| started.elapsed() >= limit);
    let stop = || spent() || ctx.halted().is_some();
    let dispatch_samples = Mutex::new(Vec::with_capacity(bins.len()));
    // A successful process whose declared inputs do not bracket its execution has not supplied
    // a stable verdict for this tree. Keep its unbounded event as a tombstone and make the
    // aggregate check COULD NOT RUN after every child has settled.
    let miss_drift = Mutex::new(Vec::new());
    // Hear one binary's end: classify its red before event I/O can hold up the local stop, emit
    // its FAIL before the halt's NOT RUN events, arm that run-wide halt, and keep its sample.
    let ended = |binary: &Binary, ran: &Ran, started: bool, halt: Option<&dyn Fn()>| {
        if ctx.release && started && ran.run.ok {
            let (command, exact_error) = commands
                .get(&binary.spec)
                .expect("every successful release miss keeps its exact command");
            let after = evidence_for(
                &new_evidence_builder(),
                binary,
                command,
                exact_error.as_deref(),
            );
            let mut evidence = evidence.lock().unwrap_or_else(PoisonError::into_inner);
            let before = evidence.get(&binary.spec).cloned();
            let stable = match before {
                Some(before) if before == after => after,
                Some(before) => {
                    miss_drift
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(binary.spec.clone());
                    Evidence {
                        exe_sha256: after.exe_sha256.or(before.exe_sha256),
                        content_key: None,
                        reason: Some(
                            "runtime inputs changed while the test binary ran; its read-only \
                             declaration was violated"
                                .into(),
                        ),
                    }
                }
                None => {
                    miss_drift
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(binary.spec.clone());
                    Evidence {
                        exe_sha256: after.exe_sha256,
                        content_key: None,
                        reason: Some(
                            "runtime inputs were not captured immediately before the test binary ran"
                                .into(),
                        ),
                    }
                }
            };
            evidence.insert(binary.spec.clone(), stable);
        }
        let evidence = evidence_of(binary);
        let (outcome, event, sample) =
            binary_event(binary, ran, started, evidence.as_ref(), watch.json);
        let red = mark_red_decided(outcome, ctx.halt.armed(), halt);
        emit(ctx, &event);
        if red {
            ctx.halt.red(check.name());
        }
        if let Some(sample) = sample {
            dispatch_samples
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((binary.spec.clone(), sample));
        }
    };
    let ran = dispatch_pooled(
        &bins,
        &plan,
        memory,
        ctx.pool.as_ref(),
        &live,
        &stop,
        |b, planned, local_halt| {
            if ctx.halted().is_some() {
                return None;
            }
            let mut env = ctx.exec_env();
            if let Some(limit) = ceiling {
                let left = limit.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return None;
                }
                env.child_ceiling = Some(left);
            }
            let (cmd, exact_error) = if ctx.release {
                let (command, exact_error) = commands
                    .get(&b.spec)
                    .cloned()
                    .expect("every release miss keeps its finalized exact command");
                (command, exact_error)
            } else {
                (
                    final_binary_cmd(ctx, b, planned.width, checkers, watch),
                    None,
                )
            };
            if ctx.release {
                let before = evidence_for(&new_evidence_builder(), b, &cmd, exact_error.as_deref());
                evidence
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(b.spec.clone(), before);
            }
            let mut run = exec::run_test(&cmd, env, local_halt);
            if cmd.test_cases.as_ref().is_some_and(|w| w.json) {
                // The raw events were the child's log; every reader takes libtest's report.
                run.output = crate::libtest_json::render(&run.output, run.stopped_at.as_ref());
            }
            Some(run)
        },
        |binary, ran, halt| ended(binary, ran, true, Some(halt)),
    );
    let total = started.elapsed();
    progress.enter(Phase::Reporting);
    // A binary the dispatch left unstarted: stopped by the run's first red, with the halt's words;
    // or by its spent ceiling, a FAIL of its own, as it always was. Each is heard as the rest
    // were, and keeps no sample.
    let ran: Vec<Option<Ran>> = bins
        .iter()
        .zip(ran)
        .enumerate()
        .map(|(index, (b, ran))| {
            if ran.is_some() {
                return ran;
            }
            let run = match (ctx.halted(), ceiling) {
                (Some(words), _) => Run {
                    case_observations: None,
                    ok: false,
                    output: String::new(),
                    code: None,
                    spawn_error: Some(words.to_owned()),
                    ..Run::default()
                },
                (None, Some(limit)) => not_started(b, limit),
                (None, None) => never_run(),
            };
            let planned = plan.order.iter().find(|p| p.index == index).copied()?;
            let left = Ran {
                run,
                planned,
                waiting: total,
                execution: Duration::ZERO,
            };
            ended(b, &left, false, None);
            Some(left)
        })
        .collect();
    let mut miss_drift = miss_drift
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    miss_drift.sort();
    miss_drift.dedup();
    let carried_drift = finish_carried();
    let carried_stable = carried.len().saturating_sub(carried_drift.len());
    let mut samples = probe_samples;
    samples.extend(
        dispatch_samples
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner),
    );
    let kept = ctx
        .test_history
        .as_deref()
        .map(|path| crate::test_history::keep(path, samples));
    let (runs, walls): (Vec<Run>, Vec<Duration>) = ran
        .iter()
        .map(|ran| match ran {
            Some(ran) => (ran.run.clone(), ran.execution),
            None => (never_run(), Duration::ZERO),
        })
        .unzip();
    // What the binaries' memory claim was, what a release's resume carried and what hashing the
    // executables cost, how the binaries used their charges and whether their history was kept,
    // in a halted run's report as in a whole one's.
    let accounted = |r: &mut Report| {
        r.raw(summary(&plan, &ran));
        claimed_said(r, claim);
        if ctx.release {
            r.raw(format!(
                "  test binaries: {} of {listed} carried (prior ok, executable and complete \
                 runtime inputs equal through dispatch); {} run; {} invalidated",
                carried_stable,
                bins.len() + probe_hung_count,
                carried_drift.len()
            ));
        }
        if ctx.release {
            let evidence = evidence.lock().unwrap_or_else(PoisonError::into_inner);
            let bounded = evidence.values().filter(|item| item.bounded()).count();
            let mut unbounded: BTreeMap<&str, usize> = BTreeMap::new();
            for reason in evidence.values().filter_map(|item| item.reason.as_deref()) {
                *unbounded.entry(reason).or_default() += 1;
            }
            r.raw(format!(
                "  test binaries' content evidence: {bounded} bounded, {} unbounded, {} \
                 executables hashed in {} summed over workers",
                listed.saturating_sub(bounded),
                evidence
                    .values()
                    .filter(|item| item.exe_sha256.is_some())
                    .count(),
                crate::report::duration(
                    Duration::from_nanos(evidenced.load(Ordering::Relaxed)).as_secs_f64()
                )
            ));
            for (reason, count) in unbounded.into_iter().take(crate::report::FINDINGS_SHOWN) {
                r.raw(format!("    unbounded ({count}): {reason}"));
            }
        }
    };
    let tail = |r: &mut Report| {
        r.raw(poles(&bins, &walls, total));
        let over = overruns(&bins, &ran);
        if !over.is_empty() {
            r.raw(over);
        }
        if let Some(Err(why)) = &kept {
            r.raw(format!(
                "  test binaries' history not kept (planning data only): {why}"
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
        accounted(r);
        tail(r);
        if !done.is_empty() {
            let (bins, runs): (Vec<Binary>, Vec<Run>) = done
                .into_iter()
                .map(|(b, run)| (b.clone(), run.clone()))
                .unzip();
            r.raw(combined(&bins, &runs).output);
            // The golden test may be among those stopped, and the run is red already.
            decide_runs(r, &bins, &runs, label, |r, rest| {
                r.decide_test_child(rest, label, &rerun);
            });
        }
        r.cannot_run(format!(
            "{label} — {} of {} test binaries not run: {words}",
            stopped.len(),
            bins.len()
        ));
        return;
    }
    if !carried_drift.is_empty() && runs.iter().all(|run| run.ok) {
        r.raw(preface);
        accounted(r);
        r.raw(combined(&bins, &runs).output);
        tail(r);
        r.cannot_run(format!(
            "{label} — {} carried candidate(s) changed after selection and were not run: {}",
            carried_drift.len(),
            carried_drift.join(", ")
        ));
        return;
    }
    if !miss_drift.is_empty() && runs.iter().all(|run| run.ok) {
        r.raw(preface);
        accounted(r);
        r.raw(combined(&bins, &runs).output);
        tail(r);
        r.cannot_run(format!(
            "{label} — runtime inputs changed while {} successful test binary(s) ran: {}",
            miss_drift.len(),
            miss_drift.join(", ")
        ));
        return;
    }
    r.raw(preface);
    accounted(r);
    r.raw(combined(&bins, &runs).output);
    tail(r);
    decide_runs(r, &bins, &runs, label, |r, rest| {
        decide(ctx, r, rest, label, &rerun);
    });
}

#[cfg(test)]
impl Ran {
    /// `run`, at width and charge one with no reading, waiting `waiting` and taking `execution`.
    fn of(run: Run, waiting: Duration, execution: Duration) -> Ran {
        Ran {
            run,
            planned: Planned {
                index: 0,
                width: 1,
                charge: 1,
                peak: 0,
                predicted: None,
            },
            waiting,
            execution,
        }
    }
}

/// Run `run(i)` for each `i` below `n` through [`dispatch`], every binary unseen on a share of
/// `n` cores, each answer in its place: for the tests of what a worker's children are handed.
#[cfg(test)]
pub(crate) fn dispatch_each<T: Send>(n: usize, run: impl Fn(usize) -> T + Sync) -> Vec<Option<T>> {
    let bins: Vec<Binary> = (0..n)
        .map(|i| Binary {
            harness: false,
            header: String::new(),
            spec: format!("t{i}"),
            record: Record {
                cwd: PathBuf::new(),
                argv: Vec::new(),
                env: Vec::new(),
            },
        })
        .collect();
    let plan = test_plan::plan(
        &vec![None; n],
        u32::try_from(n).unwrap_or(u32::MAX),
        None,
        0,
    );
    let out: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    dispatch(
        &bins,
        &plan,
        None,
        &|| 0.0,
        &|| false,
        |_, p, _| {
            *out[p.index].lock().unwrap_or_else(PoisonError::into_inner) = Some(run(p.index));
            Some(Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: true,
                output: String::new(),
                code: Some(0),
                spawn_error: None,
                usage: None,
            })
        },
        |_, _, _| {},
    );
    out.into_iter()
        .map(|m| m.into_inner().unwrap_or_else(PoisonError::into_inner))
        .collect()
}

/// A binary the dispatch never started though no ceiling halted it: a defect of this gate, and
/// a FAIL, never a pass.
fn never_run() -> Run {
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: "aterm-verify: this test binary was never run (a gate defect)\n".into(),
        code: None,
        spawn_error: None,
        usage: None,
    }
}

static RECORDING: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
#[path = "testrun_environment_tests.rs"]
mod environment_tests;

#[cfg(test)]
#[path = "testrun_discovery_tests.rs"]
mod discovery_tests;

#[cfg(all(test, unix))]
#[path = "testrun_probe_hang_tests.rs"]
mod probe_hang_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch ledger and each claim asked of it: memory, cores, requested admission mode, and
    /// whether this process held a cores claim as it was asked.
    type Scratch = Option<(
        aterm_capacity::Ledger,
        Vec<(u64, u32, aterm_capacity::Admit, bool)>,
    )>;

    thread_local! {
        /// The scratch ledger [`scratch_admit`] admits in, and the claims asked of it.
        static SCRATCH: std::cell::RefCell<Scratch> = const { std::cell::RefCell::new(None) };
    }

    /// Admit `need` at once in the thread's scratch ledger, on a machine with room for it, noting
    /// what it asked for and whether this process held cores as it asked.
    fn scratch_admit(
        need: aterm_capacity::Need,
        mode: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        SCRATCH.with_borrow_mut(|scratch| {
            let (ledger, asked) = scratch.as_mut().expect("a scratch ledger");
            let cores_held = ledger
                .of_pid(std::process::id())
                .expect("the scratch ledger reads")
                .iter()
                .any(|r| r.held.is_some_and(|h| h.cores > 0));
            asked.push((need.mem, need.cores, mode, cores_held));
            crate::admit_in(
                ledger,
                &crate::timing_test_machine::Measured::new(&aterm_capacity::Fixed::new(
                    aterm_capacity::Reading::of_gib(48, 40, 14),
                )),
                aterm_capacity::Pace::default(),
                need,
                // Unit tests record Background admission but need not own their harness's
                // process group. The directly spawned candidate verifier exercises that real
                // precondition.
                if mode == aterm_capacity::Admit::Preemptible {
                    aterm_capacity::Admit::Wait
                } else {
                    mode
                },
                &mut |_| {},
            )
        })
    }

    /// A decisive red closes the dispatch under the same mutex as a start, before its event is
    /// written: a worker that already read the external stop but has not taken that mutex finds
    /// the head still queued. The run-wide halt is armed afterward. NEGATIVE CONTROL: removing
    /// `mark_red_decided`'s synchronized halt starts that head; a non-red outcome, or a red in a
    /// run that does not stop, leaves a fresh dispatcher live.
    #[test]
    fn a_decisive_red_stops_queued_binaries_before_its_event_is_written() {
        let plan = test_plan::plan(&[None], 1, None, 1);
        let state = Mutex::new(Dispatcher::new(&plan, Some(1)));
        let turn = Condvar::new();
        let (stop_read, read_stop) = std::sync::mpsc::channel();
        let (may_lock, lock_now) = std::sync::mpsc::channel();
        let lock_now = Mutex::new(lock_now);
        let visits = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let after_stop = || {
                if visits.fetch_add(1, Ordering::SeqCst) == 0 {
                    stop_read.send(()).expect("the test hears the stop read");
                    lock_now
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(60))
                        .expect("the admission lock was not released within one minute");
                }
            };
            let admission_state = &state;
            let admission_turn = &turn;
            let attempt = scope.spawn(move || {
                admitted_after_stop(
                    admission_state,
                    admission_turn,
                    &|| 0.0,
                    &|| false,
                    &after_stop,
                )
            });
            read_stop
                .recv_timeout(Duration::from_secs(60))
                .expect("the admission did not read stop within one minute");
            let halt = || halt_dispatch(&state, &turn);
            assert!(mark_red_decided(
                Outcome::Fail(Severity::GateFailed),
                true,
                Some(&halt)
            ));
            may_lock
                .send(())
                .expect("the admission still waits to take the lock");
            assert_eq!(attempt.join().unwrap(), None);
        });
        assert_eq!(
            state.lock().unwrap().phase(0),
            Some(crate::test_plan::Phase::Queued)
        );

        let halts = AtomicUsize::new(0);
        let count_halt = || {
            halts.fetch_add(1, Ordering::SeqCst);
        };
        assert!(!mark_red_decided(Outcome::Ok, true, Some(&count_halt)));
        assert!(!mark_red_decided(
            Outcome::Fail(Severity::GateFailed),
            false,
            Some(&count_halt)
        ));
        assert_eq!(halts.load(Ordering::SeqCst), 0);

        let live = Mutex::new(Dispatcher::new(&plan, Some(1)));
        assert_eq!(
            admitted(&live, &turn, &|| 0.0, &|| false).map(|b| b.index),
            Some(0),
            "without a stopping red, the same queued binary starts"
        );
    }

    /// Empty history and a timed sample whose tree peak was unread both plan a positive cold
    /// peak and therefore a positive run claim. NEGATIVE CONTROL: leaving an unseen or zero-peak
    /// Seen at zero makes its corresponding assertion fail and restores unclaimed concurrency.
    #[test]
    fn binaries_without_a_measured_peak_get_a_positive_plan_and_claim() {
        let binary = Binary {
            harness: false,
            header: String::new(),
            spec: "-p fixture --lib".to_owned(),
            record: Record {
                cwd: PathBuf::new(),
                argv: Vec::new(),
                env: Vec::new(),
            },
        };
        let mut history = crate::test_history::History::default();
        let physical = Some(48_u64 << 30);
        for known_without_peak in [false, true] {
            if known_without_peak {
                history.push(
                    &binary.spec,
                    crate::test_history::Sample {
                        partition: None,
                        at_ms: 1,
                        outcome: "ok".to_owned(),
                        wall_ms: 1000,
                        cpu_ms: Some(1000),
                        starved_ms: Some(0),
                        peak_bytes: None,
                        width: 1,
                        tests: Some(1),
                    },
                );
            }
            let fallback = fallback_peak(&history, &[], physical);
            assert!(fallback > 0);
            let plan =
                plan_from_history(&history, std::slice::from_ref(&binary), 4, None, fallback);
            assert_eq!(plan.order[0].peak, fallback, "{plan:?}");
            assert!(plan_claim(&plan).is_some(), "{plan:?}");
            assert!(
                aterm_capacity::peak_claim(fallback) > 0,
                "an unseen binary's fallback peak is positive"
            );
        }
    }

    /// A TEST RUN CLAIMS MEMORY BEFORE CORES, every time: the recording pass's memory and then its
    /// cores, and the plan's memory only once those went back, then its cores again; so it
    /// never waits for memory while it holds cores, as a run that holds memory may wait for cores.
    /// NEGATIVE CONTROL: claims taken while the plan's are still held ask for memory beside held
    /// cores, and the admitter sees it.
    #[test]
    fn a_test_run_never_asks_for_memory_while_it_holds_cores() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        SCRATCH.set(Some((
            aterm_capacity::Ledger::at(tmp.path().join("ledger")),
            Vec::new(),
        )));
        let asked =
            || SCRATCH.with_borrow_mut(|s| std::mem::take(&mut s.as_mut().expect("scratch").1));
        let mut ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            tmp.path().join("scratch"),
        );
        ctx.memory = Some(crate::memory::Book::at(
            None,
            Some(48 << 30),
            Some(40 << 30),
        ));
        let gib = 1_u64 << 30;
        let first =
            claim_both(&ctx, "t", None, 2 * gib, "one", 6, scratch_admit).expect("admitted");
        assert!(first.resources.is_some());
        assert_eq!(first.granted, 6);
        let plan = claim_both(&ctx, "t", Some(first), 4 * gib, "plan", 6, scratch_admit)
            .expect("admitted");
        assert_eq!(
            asked(),
            [
                (2 * gib, 0, aterm_capacity::Admit::Wait, false),
                (0, 6, aterm_capacity::Admit::Wait, false),
                (4 * gib, 0, aterm_capacity::Admit::Wait, false),
                (0, 6, aterm_capacity::Admit::Wait, false)
            ]
        );
        let beside =
            claim_both(&ctx, "t", None, 2 * gib, "beside", 6, scratch_admit).expect("admitted");
        assert_eq!(
            asked(),
            [
                (2 * gib, 0, aterm_capacity::Admit::Wait, true),
                (0, 6, aterm_capacity::Admit::Wait, true)
            ]
        );
        drop((plan, beside));
        SCRATCH.set(None);
    }

    /// UNDER THE RELEASE'S POOL a test run claims its memory as ever and no cores: the pool's
    /// slots are its grant, which the release's own core claim bounds, and a claim of its own
    /// would count the release's other branches' and wait for them. NEGATIVE CONTROL: the same
    /// run with no pool asks the ledger for its cores.
    #[cfg(unix)]
    #[test]
    fn a_run_under_the_releases_pool_claims_its_memory_and_no_cores() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        SCRATCH.set(Some((
            aterm_capacity::Ledger::at(tmp.path().join("ledger")),
            Vec::new(),
        )));
        let asked =
            || SCRATCH.with_borrow_mut(|s| std::mem::take(&mut s.as_mut().expect("scratch").1));
        let release = aterm_capacity::Jobserver::new(&tmp.path().join("qualify.jobs"), 5)
            .expect("the release's pool");
        let ctx = |pool| {
            let mut ctx = crate::Ctx::new(
                tmp.path().to_path_buf(),
                crate::Scope::workspace(),
                crate::EnvSnapshot {
                    release_pool: Some(release.handed()),
                    ..crate::EnvSnapshot::default()
                },
                tmp.path().join("scratch"),
            )
            .with_pool(pool);
            ctx.memory = Some(crate::memory::Book::at(
                None,
                Some(48 << 30),
                Some(40 << 30),
            ));
            ctx
        };
        let pooled = ctx(Some(
            aterm_capacity::Jobserver::adopt(&release.handed()).expect("adopted"),
        ));
        let gib = 1_u64 << 30;
        let claims =
            claim_both(&pooled, "t", None, 2 * gib, "one", 5, scratch_admit).expect("admitted");
        assert!(claims.resources.is_some(), "its memory is claimed as ever");
        assert_eq!(claims.granted, 5, "the pool's slots are its grant");
        assert_eq!(
            asked(),
            [(2 * gib, 0, aterm_capacity::Admit::Wait, false)],
            "and no cores are asked for"
        );
        drop(claims);
        let unpooled = ctx(None);
        let claims =
            claim_both(&unpooled, "t", None, 2 * gib, "one", 5, scratch_admit).expect("admitted");
        assert_eq!(
            asked(),
            [
                (2 * gib, 0, aterm_capacity::Admit::Wait, false),
                (0, 5, aterm_capacity::Admit::Wait, false)
            ]
        );
        drop(claims);
        SCRATCH.set(None);
    }

    /// A repeatable tree's test dispatcher asks once for memory and cores together, at
    /// Background rank, and uses that one claim's actual core grant. NEGATIVE CONTROL: the
    /// ordinary test above still asks memory and cores separately; restoring that path here
    /// produces two requests and fails this test.
    #[test]
    fn a_background_test_run_claims_memory_and_cores_once() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        SCRATCH.set(Some((
            aterm_capacity::Ledger::at(tmp.path().join("ledger")),
            Vec::new(),
        )));
        let mut ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            tmp.path().join("scratch"),
        );
        ctx.background = true;
        ctx.memory = Some(crate::memory::Book::at(
            None,
            Some(48 << 30),
            Some(40 << 30),
        ));
        let gib = 1_u64 << 30;
        let claims =
            claim_both(&ctx, "t", None, 2 * gib, "one", 6, scratch_admit).expect("admitted");
        assert!(claims.resources.is_some());
        assert!(claims._cores.is_none());
        assert_eq!(claims.granted, 6);
        let asked =
            SCRATCH.with_borrow_mut(|s| std::mem::take(&mut s.as_mut().expect("scratch").1));
        assert_eq!(
            asked,
            [(2 * gib, 6, aterm_capacity::Admit::Preemptible, false)]
        );
        drop(claims);
        SCRATCH.set(None);
    }

    #[test]
    fn broken_test_home_preparation_refuses_before_test_dispatch() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let events = tmp.path().join("phases.events");
        let scratch = tmp.path().join("not-a-directory");
        std::fs::write(&scratch, b"retained").expect("block home preparation");
        let ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            scratch.clone(),
        )
        .with_events(Some(crate::event_log::EventLog::open(&events).unwrap()));
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
        }] if label.starts_with("cannot prepare release test environment:"))
        );
        assert_eq!(
            std::fs::read(&scratch).expect("retained blocker"),
            b"retained"
        );
        let phases: Vec<aterm_json::Value> = std::fs::read_to_string(events)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .map(|line| aterm_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            phases.last().unwrap()["phase"].as_str(),
            Some("preparation")
        );
        assert_eq!(
            phases.last().unwrap()["scope_exit"].as_str(),
            Some("returned")
        );
        assert!(
            !phases
                .iter()
                .any(|event| event["phase"].as_str() == Some("recording"))
        );
    }

    /// Cargo itself resolves the effective target before the recorder is installed: the
    /// environment, `CARGO_HOME`, command-local config and an explicit `--target` all refuse the
    /// native dispatcher. Diagnostic stderr is not parsed as machine JSON, and no ordinary targo
    /// command runs. NEGATIVE CONTROL: with none of those selections, Cargo's exact "not set"
    /// answer is the native default.
    #[cfg(unix)]
    #[test]
    fn every_effective_cargo_target_refuses_before_test_dispatch() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let targo = tmp.path().join("targo");
        let dispatched = tmp.path().join("dispatched");
        crate::standin_body(
            &targo,
            r#"case "$*" in
  *"config get build.target"*)
    echo 'targo: diagnostic belongs on stderr' >&2
    if [ -n "$CARGO_BUILD_TARGET" ]; then
      printf '{"build":{"target":"%s"}}\n' "$CARGO_BUILD_TARGET"
    elif [ -f "$CARGO_HOME/config.toml" ]; then
      printf '{"build":{"target":"home-target"}}\n'
    else
      case "$*" in
        *command-target*) printf '{"build":{"target":"command-target"}}\n' ;;
        *) echo 'error: config value `build.target` is not set' >&2; exit 101 ;;
      esac
    fi
    exit 0 ;;
esac
printf ran > "$TARGET_DISPATCHED"
exit 0"#,
        );
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir(&scratch).expect("verifier scratch");
        let ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            scratch,
        );
        let targets = |cmd: Cmd| {
            configured_build_targets(
                &ctx,
                &cmd.remove_env("CARGO_HOME")
                    .remove_env("CARGO_BUILD_TARGET")
                    .env("TARGET_DISPATCHED", dispatched.as_os_str()),
            )
        };

        assert_eq!(targets(Cmd::new(&targo)), Ok(None));
        assert_eq!(
            targets(Cmd::new(&targo).env("CARGO_BUILD_TARGET", "env-target")),
            Ok(Some(vec!["env-target".to_owned()]))
        );
        let cargo_home = tmp.path().join("cargo-home");
        std::fs::create_dir(&cargo_home).expect("CARGO_HOME");
        std::fs::write(
            cargo_home.join("config.toml"),
            "[build]\ntarget = \"home-target\"\n",
        )
        .expect("target config");
        assert_eq!(
            targets(Cmd::new(&targo).env("CARGO_HOME", &cargo_home)),
            Ok(Some(vec!["home-target".to_owned()]))
        );
        assert_eq!(
            targets(Cmd::new(&targo).args([
                "--unverified",
                "test",
                "--config",
                "build.target=\"command-target\"",
                "--tests",
            ])),
            Ok(Some(vec!["command-target".to_owned()]))
        );
        assert_eq!(
            targets(Cmd::new(&targo).args(["--unverified", "test", "--target=cli-target"])),
            Ok(Some(vec!["cli-target".to_owned()]))
        );
        assert!(
            !dispatched.exists(),
            "only the target query may run before refusal"
        );
    }

    /// Cargo announcing a binary without invoking the exact-host recorder refuses the run. It is
    /// never retried through Cargo, because that path would execute an un-watched test binary.
    /// NEGATIVE CONTROL: the stand-in exits successfully, so treating Cargo's status alone as a
    /// pass would fail this test.
    #[cfg(unix)]
    #[test]
    fn an_announced_binary_that_bypasses_the_recorder_is_not_run_again() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let targo = tmp.path().join("targo");
        crate::standin_body(
            &tmp.path().join("trustc"),
            "printf 'rustc 1.93.0 (trust)\\nbinary: rustc\\nhost: aarch64-apple-darwin\\nrelease: \
             1\\n'",
        );
        std::fs::write(
            &targo,
            "#!/bin/sh\ncase \"$*\" in\n  *config*get*build.target*)\n    echo 'error: \
             config value `build.target` is not set' >&2\n    exit 101 ;;\nesac\nprintf '     \
             Running unittests src/lib.rs \
             (target/debug/deps/aterm_gpu-1)\\n'\nexit 0\n",
        )
        .expect("the stand-in");
        std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let mut ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            tmp.path().to_path_buf(),
        );
        ctx.tools.stage2_dir = tmp.path().to_path_buf();
        let mut report = Report::new("the test run");
        run_tests(
            &ctx,
            &mut report,
            "fixture",
            &Cmd::new(&targo),
            crate::checks::Check::Test,
        );
        let rows: Vec<_> = report
            .entries
            .iter()
            .filter_map(|entry| match entry {
                crate::ladder::Entry::Ladder { outcome, label, .. } => {
                    Some((*outcome, label.as_str()))
                }
                _ => None,
            })
            .collect();
        assert!(
            matches!(rows.as_slice(), [(crate::ladder::Outcome::Fail(
                crate::ladder::Severity::CouldNotRun
            ), label)] if label.contains("recording runner left no record")),
            "{rows:?}"
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
        let plan = test_plan::plan(&[None, None], 2, Some(1), 0);
        let runs = dispatch(
            &bins,
            &plan,
            None,
            &|| 0.0,
            &|| false,
            |binary, _, _| {
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
                let run = Run {
                    case_observations: None,
                    stopped_at: None,
                    hung: None,
                    ok: binary.spec == "slow",
                    output: String::new(),
                    code: Some(i32::from(binary.spec != "slow")),
                    spawn_error: None,
                    usage: None,
                };
                Some(run)
            },
            |binary, _, _| {
                reported.lock().unwrap().push(binary.spec.clone());
                if binary.spec == "failed" {
                    *released.0.lock().unwrap() = true;
                    released.1.notify_all();
                }
            },
        );
        assert_eq!(*reported.lock().unwrap(), ["failed", "slow"]);
        let ok = |i: usize| runs[i].as_ref().map(|r| r.run.ok);
        assert_eq!((ok(0), ok(1)), (Some(true), Some(false)));
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
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: "test counterfeit ... FAILED".into(),
            code: Some(0),
            spawn_error: None,
            usage: None,
        };
        let parse = |run: &Run| -> aterm_json::Value {
            let (_, line, _) = binary_event(
                &binary,
                &Ran::of(run.clone(), Duration::ZERO, Duration::ZERO),
                true,
                None,
                false,
            );
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
        };
        assert_eq!(parse(&run)["outcome"].as_str(), Some("ok"));
        assert_eq!(parse(&run)["failures"].as_array().unwrap().len(), 0);
        run.ok = false;
        run.spawn_error = Some("test executable unavailable".into());
        assert_eq!(parse(&run)["outcome"].as_str(), Some("NOT RUN"));
    }

    #[test]
    fn binary_events_report_completed_counts_without_inventing_them() {
        let binary = Binary {
            harness: true,
            header: "test".into(),
            spec: "-p test --lib".into(),
            record: rec("/r/test", "test"),
        };
        let event = |ok, output: &str| {
            let run = Run {
                ok,
                code: Some(i32::from(!ok)),
                output: output.into(),
                ..Run::default()
            };
            let (_, line, _) = binary_event(
                &binary,
                &Ran::of(run, Duration::ZERO, Duration::ZERO),
                true,
                None,
                false,
            );
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()
        };
        let summary = "running 3 tests\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let passed = event(true, summary);
        assert_eq!(passed["tests"].as_u64(), Some(3));
        assert_eq!(passed["test_counts"]["passed"].as_u64(), Some(2));
        assert_eq!(passed["test_counts"]["ignored"].as_u64(), Some(1));
        assert!(
            event(false, summary)["test_counts"].is_null(),
            "the exit contradicts the reported success"
        );
        assert!(event(true, "running 3 tests\n")["test_counts"].is_null());
        let failed = "running 3 tests\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        assert_eq!(
            event(false, failed)["test_counts"]["failed"].as_u64(),
            Some(1)
        );
        assert!(
            event(true, failed)["test_counts"].is_null(),
            "a passing child cannot invent a failed count"
        );
        let carried = CarriedBinary {
            binary,
            evidence: Evidence {
                exe_sha256: Some("aa".repeat(32)),
                content_key: Some("11".repeat(32)),
                reason: None,
            },
            json: false,
            prior_execution_s: 0.01,
        };
        let changed = Evidence {
            exe_sha256: carried.evidence.exe_sha256.clone(),
            content_key: None,
            reason: Some("runtime inputs changed".into()),
        };
        for line in [
            carried_event(&carried),
            unstable_carried_event(&carried, &changed),
        ] {
            let event: aterm_json::Value =
                aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
            assert_eq!(
                event.get("test_counts"),
                Some(&aterm_json::Value::Null),
                "a retained candidate ran no tests now"
            );
        }
    }

    /// A proven hang is a bounded structured FAIL, even when the sampler's error contains the
    /// full-disk sentence that ordinarily makes a child NOT RUN. Its first-red decision invokes
    /// the local halt once. NEGATIVE CONTROL: removing the structured hang restores the
    /// environmental classification.
    #[test]
    fn a_hung_binary_is_a_bounded_named_gate_failure() {
        let binary = Binary {
            harness: true,
            header: "test".into(),
            spec: "-p test --lib".into(),
            record: rec("/r/test", "test"),
        };
        let running = (0..HUNG_CASES_SHOWN + 5)
            .map(|index| format!("case-{index}-{}", "x".repeat(1024)))
            .collect();
        let run = Run {
            case_observations: None,
            stopped_at: None,
            hung: Some(crate::exec::Hung {
                spec: binary.spec.clone(),
                binary: format!("/r/{}", "binary".repeat(200)),
                running,
                window: Duration::from_secs(180),
                sample: crate::exec::StackSample {
                    report: Some(PathBuf::from("/private/hung.partial.sample.txt")),
                    error: Some(format!(
                        "{} while sampling: {}",
                        crate::exec::ENOSPC_SENTENCE,
                        "detail".repeat(2_000)
                    )),
                },
                json: true,
            }),
            ok: false,
            output: format!("{} (os error 28)\n", crate::exec::ENOSPC_SENTENCE),
            code: None,
            spawn_error: None,
            usage: None,
        };
        assert_eq!(run.environment_failure(), None);
        let all = combined(std::slice::from_ref(&binary), std::slice::from_ref(&run));
        assert!(all.hung.is_some() && all.environment_failure().is_none());
        let (outcome, line, _) = binary_event(
            &binary,
            &Ran::of(run.clone(), Duration::ZERO, Duration::ZERO),
            true,
            None,
            true,
        );
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
        assert!(
            line.len() < 64 * 1024,
            "one atomic event line: {} bytes",
            line.len()
        );
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["outcome"].as_str(), Some("FAIL"));
        assert_eq!(event["hung"].as_bool(), Some(true));
        assert_eq!(event["hang_window_s"].as_f64(), Some(180.0));
        assert_eq!(
            event["hung_cases"].as_array().unwrap().len(),
            HUNG_CASES_SHOWN
        );
        assert_eq!(event["hung_cases_omitted"].as_u64(), Some(5));
        assert_eq!(
            event["stack_sample"].as_str(),
            Some("/private/hung.partial.sample.txt")
        );
        assert!(
            event["stack_sample_error"]
                .as_str()
                .is_some_and(|error| error.contains(crate::exec::ENOSPC_SENTENCE))
        );
        let halts = AtomicUsize::new(0);
        assert!(mark_red_decided(
            outcome,
            true,
            Some(&|| {
                halts.fetch_add(1, Ordering::SeqCst);
            })
        ));
        assert_eq!(halts.load(Ordering::SeqCst), 1);

        let mut control = run;
        control.hung = None;
        assert!(control.environment_failure().is_some());
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

    /// A resume carries a binary only when the prior ok names both its current executable and
    /// complete bounded content key, and never GOLDEN's package. Evidence is computed for every
    /// binary, including the first run and binaries absent from the prior proof. Negative
    /// controls: a changed content key and GOLDEN's exact match both run.
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
        let evidence = |b: &Binary| Evidence {
            exe_sha256: Some(match b.record.exe().to_str() {
                Some("/t/deps/aterm_db-1") => "aa".repeat(32),
                Some("/t/deps/aterm_grid-2") => "bb".repeat(32),
                Some("/t/deps/aterm_gpu-3") => "cc".repeat(32),
                _ => "dd".repeat(32),
            }),
            content_key: Some(match b.record.exe().to_str() {
                Some("/t/deps/aterm_db-1") => "11".repeat(32),
                Some("/t/deps/aterm_grid-2") => "22".repeat(32),
                Some("/t/deps/aterm_gpu-3") => "33".repeat(32),
                _ => "44".repeat(32),
            }),
            reason: None,
        };
        let mut passed = Passed::new();
        for (index, binary) in bins.iter().take(3).enumerate() {
            let mut prior =
                CarryProof::passed(&binary.spec, &evidence(binary), 1.25 + index as f64);
            if index == 1 {
                prior.content_key = Some("ee".repeat(32));
            }
            passed.insert(binary.spec.clone(), prior);
        }
        let mut failed = CarryProof::passed(&bins[3].spec, &evidence(&bins[3]), 2.0);
        failed.outcome = "FAIL".into();
        passed.insert(bins[3].spec.clone(), failed);
        let evidenced = Mutex::new(Vec::new());
        let Carried {
            run,
            carried,
            known,
        } = carry(
            &passed,
            bins,
            |b, _| {
                evidenced.lock().unwrap().push(b.spec.clone());
                evidence(b)
            },
            2,
        );
        let specs = |v: &[Binary]| v.iter().map(|b| b.spec.clone()).collect::<Vec<_>>();
        assert_eq!(
            carried
                .iter()
                .map(|carried| carried.binary.spec.as_str())
                .collect::<Vec<_>>(),
            ["-p aterm-db --lib"],
            "only the exact bounded prior ok"
        );
        assert_eq!(
            specs(&run),
            [
                "-p aterm-grid --lib",
                "-p aterm-gpu --lib",
                "-p aterm-core --lib"
            ],
            "a changed key runs, GOLDEN's package always runs, and a direct prior FAIL runs"
        );
        let mut evidenced = evidenced.into_inner().unwrap();
        evidenced.sort();
        assert_eq!(
            evidenced,
            [
                "-p aterm-core --lib",
                "-p aterm-db --lib",
                "-p aterm-gpu --lib",
                "-p aterm-grid --lib",
            ],
            "every release binary gets evidence on its first run too"
        );
        assert_eq!(
            known
                .get("-p aterm-grid --lib")
                .and_then(|evidence| evidence.content_key.as_deref()),
            Some("22".repeat(32).as_str())
        );
        let event: aterm_json::Value = aterm_json::from_str(
            carried_event(&carried[0])
                .strip_prefix("verify-event: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(event["outcome"].as_str(), Some("ok"));
        assert_eq!(event["carried"].as_bool(), Some(true));
        assert_eq!(event["execution_s"].as_f64(), Some(0.0));
        assert_eq!(event["recorded_execution_s"].as_f64(), Some(1.25));
        assert_eq!(event["exe_sha256"].as_str(), Some("aa".repeat(32).as_str()));
        assert_eq!(
            event["content_key"].as_str(),
            Some("11".repeat(32).as_str())
        );
    }

    /// Deterministic release-line timeline for A19. A cfg(test) edit changes the edited target
    /// and one dependent executable; a fixture changes only its declared reader; a harness or
    /// verifier context change changes every complete key. The fixture scheduler has one
    /// execution slot, so the sum is its modeled wall, not a production latency claim. NEGATIVE
    /// CONTROL: an otherwise equal undeclared reader still runs.
    #[test]
    fn fix_hop_line_timeline_runs_only_changed_complete_inputs() {
        let binary = |name: &str| Binary {
            harness: false,
            header: String::new(),
            spec: format!("-p {name} --lib"),
            record: rec(&format!("/t/deps/{name}-0000000000000000"), name),
        };
        let bins = vec![
            binary("edited"),
            binary("dependent"),
            binary("fixture-reader"),
            binary("fixture-sibling"),
        ];
        let mut base = BTreeMap::new();
        for (index, binary) in bins.iter().enumerate() {
            base.insert(
                binary.spec.clone(),
                Evidence {
                    exe_sha256: Some(format!("{:02x}", index + 1).repeat(32)),
                    content_key: Some(format!("{:02x}", index + 17).repeat(32)),
                    reason: None,
                },
            );
        }
        let mut prior = Passed::new();
        for binary in &bins {
            prior.insert(
                binary.spec.clone(),
                CarryProof::passed(&binary.spec, &base[&binary.spec], 1.0),
            );
        }
        let walls = BTreeMap::from([
            ("-p edited --lib", 12_u64),
            ("-p dependent --lib", 8),
            ("-p fixture-reader --lib", 4),
            ("-p fixture-sibling --lib", 2),
        ]);
        let scenario = |evidence: BTreeMap<String, Evidence>| {
            let result = carry(
                &prior,
                bins.clone(),
                |binary, _| evidence[&binary.spec].clone(),
                2,
            );
            let run = result
                .run
                .iter()
                .map(|binary| binary.spec.clone())
                .collect::<Vec<_>>();
            let carried = result
                .carried
                .iter()
                .map(|binary| binary.binary.spec.clone())
                .collect::<Vec<_>>();
            let single_slot_wall_s = run.iter().map(|spec| walls[spec.as_str()]).sum::<u64>();
            (run, carried, single_slot_wall_s)
        };

        let mut cfg_test = base.clone();
        for (spec, byte) in [("-p edited --lib", "a1"), ("-p dependent --lib", "a2")] {
            let evidence = cfg_test.get_mut(spec).unwrap();
            evidence.exe_sha256 = Some(byte.repeat(32));
            evidence.content_key = Some(byte.repeat(32));
        }
        let (run, carried, wall) = scenario(cfg_test);
        assert_eq!(run, ["-p edited --lib", "-p dependent --lib"]);
        assert_eq!(
            carried,
            ["-p fixture-reader --lib", "-p fixture-sibling --lib"]
        );
        assert_eq!(wall, 20);

        let mut fixture = base.clone();
        fixture
            .get_mut("-p fixture-reader --lib")
            .unwrap()
            .content_key = Some("b1".repeat(32));
        let (run, carried, wall) = scenario(fixture);
        assert_eq!(run, ["-p fixture-reader --lib"]);
        assert_eq!(
            carried,
            [
                "-p edited --lib",
                "-p dependent --lib",
                "-p fixture-sibling --lib"
            ]
        );
        assert_eq!(wall, 4);

        let harness = base
            .iter()
            .map(|(spec, evidence)| {
                let mut evidence = evidence.clone();
                evidence.content_key = Some("c1".repeat(32));
                (spec.clone(), evidence)
            })
            .collect();
        let (run, carried, wall) = scenario(harness);
        assert_eq!(run.len(), 4);
        assert!(carried.is_empty());
        assert_eq!(wall, 26);

        let mut unbounded = base.clone();
        unbounded.insert(
            "-p fixture-sibling --lib".into(),
            Evidence {
                exe_sha256: base["-p fixture-sibling --lib"].exe_sha256.clone(),
                content_key: None,
                reason: Some("runtime inputs are undeclared".into()),
            },
        );
        let (run, _, _) = scenario(unbounded);
        assert_eq!(run, ["-p fixture-sibling --lib"]);
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
        let evidence = Evidence {
            exe_sha256: Some("aa".repeat(32)),
            content_key: Some("11".repeat(32)),
            reason: None,
        };
        let event = carried_event(&CarriedBinary {
            binary,
            evidence,
            json: false,
            prior_execution_s: 1.0,
        });
        let initial = std::fs::read_to_string(&at).unwrap();
        let prepared = ctx.events.as_ref().unwrap().prepare(&event);
        emit(&ctx, &event);
        assert_eq!(
            std::fs::read_to_string(&at).unwrap(),
            format!("{initial}{prepared}")
        );
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
            runner_config(
                Path::new("/a b/aterm-verify"),
                Path::new("/tmp/q\"d"),
                "aarch64-apple-darwin",
            )
            .as_deref(),
            Some(
                "target.aarch64-apple-darwin.runner=[\"/a b/aterm-verify\",\"--record-test-binary\",\
                 \"/tmp/q\\\"d\"]"
            )
        );
        assert_eq!(
            runner_config(Path::new("/gate"), Path::new("/tmp/r"), "bad target"),
            None,
            "an invalid target cannot broaden the runner to another target"
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
            recording_cmd(&serial, "CFG", "aarch64-apple-darwin").argv(),
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
        let removed = &recording_cmd(&serial, "CFG", "aarch64-apple-darwin").remove_envs;
        assert!(removed.contains(&OsString::from("CARGO_BUILD_TARGET")));
        assert!(removed.contains(&OsString::from("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER")));
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
        let command = recording_cmd(&serial, "RUNNER", "aarch64-apple-darwin");
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

    /// THE DISPATCH: a binary starts only while its charge fits in what the running ones leave of
    /// the share, at its planned width, every binary once, and the answers in declared order
    /// whatever order they ended in. Each of the first three waits until all three run (3 + 2 + 1
    /// cores of 6), so the three ran at once, and no four fit. NEGATIVE CONTROL: had the fourth (3
    /// more) not waited for room, the share would have been passed at its start.
    #[test]
    fn binaries_run_while_their_charges_fit_and_answer_in_declared_order() {
        let charges = [3, 2, 1, 3, 2, 1, 3, 2, 1];
        let bins: Vec<Binary> = (0..charges.len())
            .map(|i| Binary {
                harness: false,
                header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let plan = Plan {
            share: 6,
            target: Duration::ZERO,
            order: charges
                .iter()
                .enumerate()
                .map(|(index, &charge)| Planned {
                    index,
                    width: charge,
                    charge,
                    peak: 0,
                    predicted: Some(Duration::from_secs(1)),
                })
                .collect(),
        };
        let held = Mutex::new((0_u32, 0_u32));
        let started = (Mutex::new(0_usize), Condvar::new());
        let ran = dispatch(
            &bins,
            &plan,
            None,
            &|| 0.0,
            &|| false,
            |b, planned, _| {
                {
                    let mut held = held.lock().unwrap();
                    held.0 += planned.charge;
                    held.1 = held.1.max(held.0);
                    assert!(held.0 <= 6, "{} started past the share", b.spec);
                }
                *started.0.lock().unwrap() += 1;
                started.1.notify_all();
                if planned.index < 3 {
                    let (count, waited) = started
                        .1
                        .wait_timeout_while(
                            started.0.lock().unwrap(),
                            aterm_spec::wait::MUST_HAPPEN,
                            |n| *n < 3,
                        )
                        .unwrap();
                    assert!(
                        *count >= 3 && !waited.timed_out(),
                        "the first three never ran together"
                    );
                }
                held.lock().unwrap().0 -= planned.charge;
                let run = Run {
                    case_observations: None,
                    stopped_at: None,
                    hung: None,
                    ok: true,
                    output: format!("{}\nran {} at {}\n", b.header, b.spec, planned.width),
                    code: Some(0),
                    spawn_error: None,
                    usage: None,
                };
                Some(run)
            },
            |_, _, _| {},
        );
        assert_eq!(held.lock().unwrap().1, 6, "the first three ran at once");
        let said: Vec<String> = ran
            .iter()
            .map(|r| {
                r.as_ref()
                    .unwrap()
                    .run
                    .output
                    .lines()
                    .nth(1)
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            said,
            (0..charges.len())
                .map(|i| format!("ran -p x --test t{i} at {}", charges[i]))
                .collect::<Vec<_>>(),
            "declared order, each at its width"
        );
        assert_eq!(most_at_once(&ran), 3, "the first three ran at once");
    }

    /// A RUN THAT PANICS (a gate defect) gives its binary's charge back and halts the dispatch:
    /// the other worker, waiting for the share, starts nothing more, and the panic reaches the
    /// caller instead of that worker waiting forever. NEGATIVE CONTROL: with no panic, every
    /// binary runs, one at a time on two cores.
    #[test]
    fn a_run_that_panics_gives_its_charge_back_and_halts_the_dispatch() {
        let bins: Vec<Binary> = (0..3)
            .map(|i| Binary {
                harness: false,
                header: format!("t{i}"),
                spec: format!("t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        // Three binaries two wide on two cores: one at a time, on two workers.
        let plan = test_plan::plan(&[None, None, None], 2, None, 0);
        let go = |panics: bool| {
            let (bins, plan) = (bins.clone(), plan.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(
                        &bins,
                        &plan,
                        None,
                        &|| 0.0,
                        &|| false,
                        |b, _, _| {
                            assert!(!(panics && b.spec == "t0"), "a gate defect");
                            Some(Run {
                                case_observations: None,
                                stopped_at: None,
                                hung: None,
                                ok: true,
                                output: String::new(),
                                code: Some(0),
                                spawn_error: None,
                                usage: None,
                            })
                        },
                        |_, _, _| {},
                    )
                    .iter()
                    .flatten()
                    .count()
                }));
                tx.send(ran.ok()).ok();
            });
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the dispatch ended")
        };
        assert_eq!(go(false), Some(3));
        assert_eq!(go(true), None, "the panic reached the caller");
    }

    /// THE RUN'S READINGS: the most binaries that ran at once are the most whose spans overlap,
    /// one ending as the next starts counted apart, and binaries left unstarted (no execution)
    /// not at all; a binary whose CPU over its wall passed its charge is named with the cores it
    /// used. NEGATIVE CONTROLS: a third binary starting before the second ends makes three at
    /// once, and a binary within its charge is not named.
    #[test]
    fn the_most_at_once_and_the_overruns_are_read_from_the_runs() {
        let s = Duration::from_secs;
        let ran = |charge: u32, waiting: u64, execution: u64, cpu: u64| {
            let mut ran = Ran::of(
                Run {
                    case_observations: None,
                    stopped_at: None,
                    hung: None,
                    ok: true,
                    output: String::new(),
                    code: Some(0),
                    spawn_error: None,
                    usage: None,
                },
                s(waiting),
                s(execution),
            );
            ran.planned.charge = charge;
            ran.run.usage = Some(exec::TreeUsage {
                cpu: s(cpu),
                ..exec::TreeUsage::default()
            });
            Some(ran)
        };
        let apart = [ran(1, 0, 2, 2), ran(2, 1, 2, 12), ran(1, 3, 1, 1)];
        assert_eq!(most_at_once(&apart), 2);
        let overlapping = [ran(1, 0, 2, 2), ran(2, 1, 2, 12), ran(1, 1, 2, 1)];
        assert_eq!(most_at_once(&overlapping), 3);
        // Three a halt left unstarted, answered at the dispatch's end with no execution: none ran.
        let halted = [
            ran(1, 0, 2, 2),
            ran(1, 2, 0, 0),
            ran(1, 2, 0, 0),
            ran(1, 2, 0, 0),
        ];
        assert_eq!(most_at_once(&halted), 1);
        let bins: Vec<Binary> = (0..3)
            .map(|i| Binary {
                harness: false,
                header: format!("t{i}"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let over = overruns(&bins, &apart);
        assert!(
            over.contains("-p x --test t1 used 6.0 cores of 2 charged"),
            "{over}"
        );
        assert!(!over.contains("t0") && !over.contains("t2"), "{over}");
        assert_eq!(overruns(&bins[..1], &apart[..1]), "");
    }

    /// A RUN'S CEILING SPENT halts the dispatch: nothing more starts, and what it left is
    /// unanswered (the run reports it as never started), as is one whose run answered that it did
    /// not start. NEGATIVE CONTROL: unspent, every binary runs.
    #[test]
    fn a_spent_ceiling_starts_nothing_more() {
        let bins: Vec<Binary> = (0..4)
            .map(|i| Binary {
                harness: false,
                header: format!("t{i}"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let plan = test_plan::plan(&[None, None, None, None], 1, None, 0);
        let done = AtomicUsize::new(0);
        let ok = || {
            Some(Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: true,
                output: String::new(),
                code: Some(0),
                spawn_error: None,
                usage: None,
            })
        };
        let spent = || done.load(Ordering::SeqCst) >= 2;
        let ran = dispatch(
            &bins,
            &plan,
            None,
            &|| 0.0,
            &spent,
            |_, _, _| ok(),
            |_, _, _| {
                done.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(
            ran.iter().map(Option::is_some).collect::<Vec<_>>(),
            [true, true, false, false]
        );
        let late = dispatch(
            &bins,
            &plan,
            None,
            &|| 0.0,
            &|| false,
            |_, p, _| if p.index == 1 { None } else { ok() },
            |_, _, _| {},
        );
        assert_eq!(
            late.iter().map(Option::is_some).collect::<Vec<_>>(),
            [true, false, true, true],
            "one its ceiling met as its turn came is answered as unstarted, and the rest go on"
        );
        let all = dispatch(
            &bins,
            &plan,
            None,
            &|| 0.0,
            &|| false,
            |_, _, _| ok(),
            |_, _, _| {},
        );
        assert!(all.iter().all(Option::is_some));
    }

    /// Binaries of `charges`, all unseen, each its charge wide, in a plan on `share` cores.
    #[cfg(unix)]
    fn charged(charges: &[u32], share: u32) -> (Vec<Binary>, Plan) {
        let bins = (0..charges.len())
            .map(|i| Binary {
                harness: false,
                header: format!("t{i}"),
                spec: format!("-p x --test t{i}"),
                record: rec(&format!("/r/t{i}"), "x"),
            })
            .collect();
        let order = charges
            .iter()
            .enumerate()
            .map(|(index, &charge)| Planned {
                index,
                width: charge,
                charge,
                peak: 0,
                predicted: None,
            })
            .collect();
        let plan = Plan {
            share,
            target: Duration::ZERO,
            order,
        };
        (bins, plan)
    }

    /// UNDER THE RELEASE'S POOL a binary holds its charge in the pool's tokens while it runs. On
    /// four slots with the release's build holding two, two binaries charged two each fit the
    /// share together, yet run one at a time on the two tokens left: while each runs, no token
    /// is free, and every token comes back. NEGATIVE CONTROL: a dispatch that takes no tokens
    /// runs a binary with two free.
    #[cfg(unix)]
    #[test]
    fn a_pooled_binary_holds_its_charge_in_the_releases_tokens_while_it_runs() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 4).expect("a pool");
        let build: Vec<_> = (0..2)
            .map(|_| pool.try_acquire().expect("reads").expect("a token"))
            .collect();
        let (bins, plan) = charged(&[2, 2], 4);
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let ran = dispatch_pooled(
            &bins,
            &plan,
            None,
            Some(&pool),
            &|| 0.0,
            &|| false,
            |_, _, _| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                assert!(
                    pool.try_acquire().expect("reads").is_none(),
                    "the build's two and this binary's two hold every token"
                );
                running.fetch_sub(1, Ordering::SeqCst);
                Some(Run {
                    case_observations: None,
                    ok: true,
                    code: Some(0),
                    ..Run::default()
                })
            },
            |_, _, _| {},
        );
        assert!(ran.iter().all(Option::is_some), "both ran");
        assert_eq!(
            most.load(Ordering::SeqCst),
            1,
            "on two tokens, one at a time"
        );
        drop(build);
        let back: Vec<_> = std::iter::from_fn(|| pool.try_acquire().expect("reads")).collect();
        assert_eq!(back.len(), 4, "every token came back");
    }

    /// A binary whose tokens the stop ends the wait for never runs and is left unstarted, as the
    /// stop leaves every binary, and the part of its charge it took goes back. The build holds
    /// one of two slots; the binary, charged both, is admitted (its `live` read), and the stop
    /// comes while it waits for the second. NEGATIVE CONTROL: a dispatch that takes no tokens runs
    /// it once the share admits it.
    #[cfg(unix)]
    #[test]
    fn a_binary_whose_tokens_never_come_is_left_unstarted_and_never_run() {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 2).expect("a pool");
        let build = pool.try_acquire().expect("reads").expect("a token");
        let (bins, plan) = charged(&[2], 2);
        let admitted = (Mutex::new(false), Condvar::new());
        let stopped = std::sync::atomic::AtomicBool::new(false);
        let live = || {
            *admitted.0.lock().unwrap() = true;
            admitted.1.notify_all();
            0.0
        };
        let started = AtomicUsize::new(0);
        let ran = std::thread::scope(|scope| {
            scope.spawn(|| {
                let (seen, waited) = admitted
                    .1
                    .wait_timeout_while(
                        admitted.0.lock().unwrap(),
                        aterm_spec::wait::MUST_HAPPEN,
                        |seen| !*seen,
                    )
                    .unwrap();
                assert!(
                    *seen && !waited.timed_out(),
                    "the binary was never admitted"
                );
                stopped.store(true, Ordering::SeqCst);
            });
            dispatch_pooled(
                &bins,
                &plan,
                None,
                Some(&pool),
                &live,
                &|| stopped.load(Ordering::SeqCst),
                |_, _, _| {
                    started.fetch_add(1, Ordering::SeqCst);
                    Some(Run::default())
                },
                |_, _, _| {},
            )
        });
        assert_eq!(started.load(Ordering::SeqCst), 0, "it never ran");
        assert!(ran[0].is_none(), "the stop left it unstarted");
        let free = pool.try_acquire().expect("reads");
        assert!(free.is_some(), "the token it took went back");
        assert!(
            pool.try_acquire().expect("reads").is_none(),
            "the build's is its own"
        );
        drop((free, build));
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
        let found = crate::findings::test_findings(
            "targo test --tests",
            &all.output,
            &crate::findings::Rerun::default(),
        );
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            &Ran::of(runs[1].clone(), Duration::ZERO, Duration::ZERO),
            true,
            None,
            false,
        );
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed), "a proven red");
        let event: aterm_json::Value =
            aterm_json::from_str(event.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["failures"][0]["case"].as_str(), Some("t::broke"));
        assert_eq!(
            event["failures"][0]["output"].as_str(),
            Some(tests[0].block.as_str())
        );
        let findings = crate::findings::test_findings(
            "targo test --tests",
            &all.output,
            &crate::findings::Rerun::default(),
        );
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
                r.decide_test_child(rest, label, &crate::findings::Rerun::default());
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
            case_observations: None,
            stopped_at: None,
            hung: None,
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
            r.decide_test_child(rest, label, &crate::findings::Rerun::default());
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
        b.record
            .env
            .push((aterm_capacity::RELEASE_RUN_ENV.into(), "stale".into()));
        b.record
            .env
            .push(("ATERM_RELEASE_INPUTS_SEAL".into(), "/owner/seal".into()));
        let cmd = binary_cmd(&b, 7, false, Watch::default());
        assert!(
            cmd.envs
                .iter()
                .all(|(key, _)| !key.as_encoded_bytes().starts_with(b"ATERM_RELEASE_"))
        );
        let json = binary_cmd(
            &b,
            7,
            false,
            Watch {
                json: true,
                stop_at_red: true,
                seed: None,
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
                "--report-time",
                "--skip",
                "slow::"
            ]
        );
        assert_eq!(
            json.test_cases,
            Some(crate::exec::CaseWatch {
                spec: "-p x --test a".into(),
                binary: "/r/target/debug/deps/a-1".into(),
                json: true,
                stop_at_red: true,
                hang_after: crate::exec::TEST_BINARY_HANG,
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
                    stop_at_red: true,
                    seed: None,
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
            seed: None,
        };
        for json in [false, true] {
            let seeded = Watch {
                json,
                seed: Some(17),
                ..armed
            };
            let command = binary_cmd(&b, 3, false, seeded);
            assert!(command.args.iter().any(|arg| arg == "--shuffle-seed=17"));
            assert!(
                command
                    .args
                    .iter()
                    .any(|arg| arg.to_string_lossy().starts_with("-Z"))
            );
            assert!(
                command
                    .envs
                    .iter()
                    .any(|(k, v)| k == "RUST_TEST_THREADS" && v == "3")
            );
            let mut custom = b.clone();
            custom.harness = false;
            assert_eq!(binary_cmd(&custom, 3, false, seeded).args, cmd.args);
        }
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

    /// Content evidence is made from the command after the full-set plan and JSON probe have
    /// fixed it. NEGATIVE CONTROLS: changing either the actual planned width or the probe's final
    /// JSON mode changes the key; hard-coding a width or keying the pre-probe command fails this.
    #[test]
    fn planned_width_and_final_json_mode_are_content_key_inputs() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("workspace");
        let package = root.join("crates/p");
        let scratch = root.join("scratch");
        let executable = root.join("target/debug/deps/p-0000000000000000");
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/p\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::write(root.join("trust.lock"), "build = 1\n").unwrap();
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"p\"\nversion = \"0.0.0\"\n\
             [package.metadata.aterm.test-inputs]\n\
             bounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = []\n",
        )
        .unwrap();
        std::fs::write(package.join("src/lib.rs"), "pub fn answer() -> u8 { 1 }\n").unwrap();
        std::fs::write(&executable, b"test executable").unwrap();
        let binary = Binary {
            header: format!(
                "     Running unittests src/lib.rs ({})",
                executable.display()
            ),
            spec: "-p p --lib".into(),
            record: Record {
                cwd: package.clone(),
                argv: vec![executable.into_os_string()],
                env: vec![
                    ("CARGO_PKG_NAME".into(), "p".into()),
                    ("CARGO_MANIFEST_DIR".into(), package.into_os_string()),
                    ("PATH".into(), "/usr/bin:/bin".into()),
                ],
            },
            harness: true,
        };
        let ctx = crate::Ctx::new(
            root,
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            scratch.clone(),
        );
        let builder = crate::test_content::EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            scratch,
        );
        let keyed = |width, json, seed| {
            let command = final_binary_cmd(
                &ctx,
                &binary,
                width,
                false,
                Watch {
                    json,
                    stop_at_red: false,
                    seed,
                },
            );
            let command = exec::finalize_exact_command(command, ctx.exec_env())
                .expect("a replayed test command is exact");
            (builder.evidence(&binary, &command), command)
        };
        let (narrow, narrow_command) = keyed(2, false, None);
        let (wide, _) = keyed(7, false, None);
        let (json, json_command) = keyed(2, true, None);
        let (seeded, seeded_command) = keyed(2, false, Some(17));
        assert!(narrow.bounded(), "{narrow:?}");
        assert_ne!(narrow.content_key, wide.content_key, "planned width");
        assert_ne!(narrow.content_key, json.content_key, "final JSON mode");
        assert_ne!(narrow.content_key, seeded.content_key, "shuffle seed");
        assert!(
            narrow_command
                .argv()
                .iter()
                .all(|argument| argument != "json")
                && json_command
                    .argv()
                    .iter()
                    .any(|argument| argument == "json")
        );
        assert!(
            seeded_command
                .argv()
                .iter()
                .any(|argument| argument == "--shuffle-seed=17")
        );
    }

    /// Carry projects the already-claimed full plan; it never widens or reorders the misses.
    /// NEGATIVE CONTROL: asking the planner again after removing the middle binary would not
    /// preserve these deliberately distinct widths, charges and predictions.
    #[test]
    fn carry_projects_the_fixed_full_plan_without_replanning() {
        let binary = |name: &str| Binary {
            header: String::new(),
            spec: format!("-p {name} --lib"),
            record: rec(&format!("/r/target/debug/deps/{name}-1"), name),
            harness: true,
        };
        let full = vec![binary("a"), binary("carried"), binary("b")];
        let plan = Plan {
            share: 8,
            target: Duration::from_secs(9),
            order: vec![
                Planned {
                    index: 2,
                    width: 7,
                    charge: 8,
                    peak: 700,
                    predicted: Some(Duration::from_secs(3)),
                },
                Planned {
                    index: 1,
                    width: 5,
                    charge: 6,
                    peak: 500,
                    predicted: Some(Duration::from_secs(5)),
                },
                Planned {
                    index: 0,
                    width: 2,
                    charge: 3,
                    peak: 200,
                    predicted: Some(Duration::from_secs(7)),
                },
            ],
        };
        let kept = vec![full[0].clone(), full[2].clone()];
        assert_eq!(
            project_plan(&plan, &full, &kept),
            Plan {
                share: 8,
                target: Duration::from_secs(9),
                order: vec![
                    Planned {
                        index: 1,
                        width: 7,
                        charge: 8,
                        peak: 700,
                        predicted: Some(Duration::from_secs(3)),
                    },
                    Planned {
                        index: 0,
                        width: 2,
                        charge: 3,
                        peak: 200,
                        predicted: Some(Duration::from_secs(7)),
                    },
                ],
            }
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
