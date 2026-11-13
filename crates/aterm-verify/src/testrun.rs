// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running test binaries concurrently: each at the width its runs call for, as many at once as
//! their charges fit in the run's CPU share ([`crate::test_plan`]).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::exec::{self, Cmd, Run};
use crate::ladder::{Outcome, Report, Severity};
use crate::test_plan::{self, Dispatcher, Plan, Planned};
use crate::test_progress::{Phase, Progress};

use crate::resource_waits as test_waits;

pub use crate::test_content::{
    CarryProof, Evidence, KeyParts, Passed, encode_passed, moved_parts, read_passed,
};

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
    /// Whether its turn came and its runner was asked to start it: `false` for one the run's
    /// first red, its spent ceiling or the release's pool left unstarted, which used no CPU.
    pub started: bool,
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
/// ([`aterm_capacity::Jobserver::try_acquire`], returning every partial attempt) before its clock
/// starts, its wait queue time ([`Ran::waiting`]), and gives them back as its process ends,
/// before `completed` and before its share. The tokens the running binaries hold are thus their
/// charges, and with the pool's other holders never more than its slots. A wait `stop()` ends
/// leaves the binary unstarted, as the stop leaves every other. A FIFO error answers it as a
/// binary verify could not start ([`Run::spawn_error`]), never run and never started
/// ([`Ran::started`]).
///
/// LOST TOKENS. A GNU client that dies holding a token takes it with it, and the pool cannot
/// name who. So a binary whose whole charge the pool does not hold, while no binary of this
/// dispatch runs and no child of the run is alive (`idle`, [`exec::no_child_runs`] in a run),
/// waits for nobody: it starts on the tokens there are, at least one, its width and its cores
/// within them (`BinaryPermit::granted`), and a line says so. It holds only what the pool had,
/// so no other holder is overdrawn. This dispatch, outside a run, lists no children and waits.
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
    // A dispatch outside a run lists no children, so it never reads a missing token as lost.
    dispatch_resources(
        bins,
        plan,
        memory,
        pool,
        None,
        live,
        stop,
        &|| false,
        run_one,
        completed,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "dispatch inputs and optional live resource admission"
)]
fn dispatch_resources<F, E>(
    bins: &[Binary],
    plan: &Plan,
    memory: Option<u64>,
    pool: Option<&aterm_capacity::Jobserver>,
    ctx: Option<&crate::Ctx>,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    idle: &(dyn Fn() -> bool + Sync),
    run_one: F,
    completed: E,
) -> Vec<Option<Ran>>
where
    F: Fn(&Binary, &Planned, &(dyn Fn() + Sync)) -> Option<Run> + Sync,
    E: Fn(&Binary, &Ran, &dyn Fn()) + Sync,
{
    let answers: Vec<Mutex<Option<Ran>>> = bins.iter().map(|_| Mutex::new(None)).collect();
    let state = Mutex::new(Dispatcher::new(plan, memory));
    let probes = Mutex::new(AdmissionPass::default());
    let turn = Condvar::new();
    let waiters = BinaryWaiters::default();
    let halt = || {
        halt_dispatch(&state, &turn);
        waiters.clear();
    };
    let meter = crate::memory::current();
    let cover = crate::memory::cover();
    let reservation = crate::current_resource_claim();
    let queued = Instant::now();
    let waits = test_waits::Trace::new(ctx, bins, plan, queued);
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
                loop {
                    let next = admitted_holding_observed(
                        &state,
                        &turn,
                        live,
                        stop,
                        &|| {
                            if stop() || exec::ending() {
                                waiters.clear();
                            }
                        },
                        &mut |generation, b, alone| {
                            let (permit, reason) = binary_permit_observed(
                                ctx,
                                pool,
                                b,
                                &waiters,
                                alone && idle(),
                                Some((&probes, generation)),
                            );
                            waits.change(b.index, reason);
                            permit
                        },
                        &mut |b, resources| {
                            waits.change(b.index, Some(test_waits::Reason::local(b, resources)))
                        },
                    );
                    let Some((planned, permit)) = next else {
                        waits.finish_pending();
                        break;
                    };
                    // What it runs on is what its ledger claim or pool tokens grant.
                    let b = execution_grant(&state, &permit, planned);
                    if let (true, Some(binary)) = (b.charge < planned.charge, bins.get(b.index)) {
                        let source = if pool.is_some() {
                            "release pool"
                        } else {
                            "capacity ledger"
                        };
                        eprintln!(
                            "verify: {} starts on {} of its {} requested cores: the {source} \
                             grant bounds its execution width",
                            binary.spec, b.charge, planned.charge
                        );
                    }
                    let _charged = Charged {
                        state: &state,
                        turn: &turn,
                        index: b.index,
                    };
                    // The next may fit beside it.
                    turn.notify_all();
                    if let Some(binary) = bins.get(b.index) {
                        let (permit, refused) = match permit {
                            Ok(permit) => (Some(permit), None),
                            Err(why) => (None, Some(unpaid_run(&why))),
                        };
                        let _binary_reservation = crate::holding_resource_claim(
                            permit
                                .as_ref()
                                .and_then(|p| p.claim.clone())
                                .or_else(|| reservation.clone()),
                        );
                        let _binary_cover = crate::memory::covering(
                            permit
                                .as_ref()
                                .and_then(|p| p.claim.as_ref())
                                .map(|c| Arc::from(c.record().cover_marker()))
                                .or_else(|| cover.clone()),
                        );
                        let started = Instant::now();
                        let asked = refused.is_none();
                        let answered = refused.or_else(|| run_one(binary, &b, &halt));
                        drop(_binary_cover);
                        drop(_binary_reservation);
                        drop(permit);
                        if let Some(run) = answered {
                            let ran = Ran {
                                run,
                                started: asked,
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

/// Actual per-binary custody. Memory is acquired before pool tokens. A failed permit drops
/// held resources; [`BinaryWaiters`] retains only its pending admission request.
struct BinaryPermit<'a> {
    claim: Option<Arc<aterm_capacity::Claim>>,
    /// The pool's tokens it holds: `None` with no pool, where its claim's cores are its charge.
    tokens: Option<Vec<aterm_capacity::Token<'a>>>,
}

/// Bind an admitted binary's execution budget to the resources it actually acquired.
fn execution_grant(
    state: &Mutex<Dispatcher>,
    permit: &Result<BinaryPermit<'_>, std::io::Error>,
    planned: Planned,
) -> Planned {
    let actual = permit
        .as_ref()
        .map_or(planned, |permit| permit.granted(planned));
    if actual.charge < planned.charge {
        state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .limit_charge(actual.index, actual.charge)
            .expect("an admitted binary retains its positive external grant");
    }
    actual
}

impl BinaryPermit<'_> {
    /// Cap the preferred charge and width by the pool tokens held, or by the ledger's
    /// actual elastic core grant when there is no pool.
    fn granted(&self, b: Planned) -> Planned {
        let Some(held) = self
            .tokens
            .as_ref()
            .and_then(|tokens| u32::try_from(tokens.len()).ok())
            .or_else(|| self.claim.as_ref().map(|claim| claim.cores()))
            .filter(|held| (1..b.charge).contains(held))
        else {
            return b;
        };
        Planned {
            charge: held,
            width: b.width.min(held),
            ..b
        }
    }
}

/// Keep one stable admission request while trying the dispatch's other safe envelopes.
/// One-shot attempts alone lose every queue position on a shortfall. Retaining the whole
/// plan would reserve every unstarted binary against later work instead of one next turn.
#[derive(Default)]
struct BinaryWaiters {
    pending: Mutex<Option<(usize, aterm_capacity::admit::Waiter)>>,
}

impl BinaryWaiters {
    fn clear(&self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(pending);
    }

    #[cfg(test)]
    fn poll_with(
        &self,
        index: usize,
        need: aterm_capacity::Need,
        machine: &dyn aterm_capacity::Machine,
        enter: impl FnOnce(
            aterm_capacity::Need,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
        self.poll_noting(index, need, machine, enter, &mut |_| None, &mut None)
    }

    fn poll_noting(
        &self,
        index: usize,
        need: aterm_capacity::Need,
        machine: &dyn aterm_capacity::Machine,
        enter: impl FnOnce(
            aterm_capacity::Need,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
        rejection: &mut impl FnMut(
            &aterm_capacity::Need,
        ) -> Option<aterm_capacity::admit::ProbeRejection>,
        waiting: &mut Option<test_waits::Reason>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
        use aterm_capacity::admit::Polled;
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        // A retained waiter keeps its queue position and always polls fresh. Only temporary
        // alternative envelopes may be rejected by the bounded shared observation.
        if pending.as_ref().is_some_and(|(queued, _)| *queued != index)
            && let Some(rejection) = rejection(&need)
        {
            *waiting = Some(match rejection {
                aterm_capacity::admit::ProbeRejection::Waiting(short) => {
                    test_waits::Reason::ledger(&short)
                }
                aterm_capacity::admit::ProbeRejection::Unreachable(why) => {
                    test_waits::Reason::unreachable(why)
                }
            });
            return Ok(None);
        }
        if let Some(why) = aterm_capacity::admit::never(&need, &machine.read(need.path.as_deref()))
        {
            *waiting = Some(test_waits::Reason::unreachable(why));
            return Ok(None);
        }
        let waiter = if pending.as_ref().is_some_and(|(queued, _)| *queued == index) {
            pending.take().expect("the queued request was present").1
        } else {
            enter(need)?
        };
        match waiter.poll(machine) {
            Polled::Admitted(claim) => Ok(Some(claim)),
            Polled::Waiting(waiter, short) => {
                *waiting = Some(test_waits::Reason::ledger(&short));
                if pending.is_none() {
                    *pending = Some((index, waiter));
                }
                Ok(None)
            }
            Polled::Gone(why) => Err(aterm_capacity::Error::Ledger(why)),
            Polled::Unqualified(why) => Err(aterm_capacity::Error::Timing(why)),
        }
    }
}

/// Background cancellation must cover the intervals with no admitted binary too. This guard
/// reserves neither memory nor CPU; per-binary custody alone owns those resources.
fn dispatch_lifetime(
    ctx: &crate::Ctx,
    admit: crate::Admitter,
) -> Result<Option<aterm_capacity::Claim>, String> {
    if !ctx.background || ctx.memory.is_none() {
        return Ok(None);
    }
    admit(
        aterm_capacity::Need::new("verify test dispatch lifetime"),
        aterm_capacity::Admit::Preemptible,
    )
    .map(Some)
    .map_err(|why| format!("test dispatch lifetime admission refused: {why}"))
}

#[derive(Default)]
struct AdmissionPass {
    generation: Option<u64>,
    sampled: Option<Instant>,
    probe: Option<aterm_capacity::admit::ResourceProbe>,
}

impl AdmissionPass {
    fn rejection(
        &mut self,
        generation: u64,
        ledger: &aterm_capacity::Ledger,
        machine: &dyn aterm_capacity::Machine,
        need: &aterm_capacity::Need,
        clock: &dyn Fn() -> Instant,
    ) -> Option<aterm_capacity::admit::ProbeRejection> {
        let at = clock();
        if self.generation != Some(generation)
            || self.sampled.is_none_or(|sampled| {
                at.saturating_duration_since(sampled)
                    >= aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE
            })
        {
            self.generation = Some(generation);
            self.sampled = Some(at);
            self.probe = aterm_capacity::admit::ResourceProbe::capture_at(ledger, machine, at)
                .ok()
                .flatten();
        }
        self.probe
            .as_ref()
            .and_then(|probe| probe.rejection_at(need, clock()))
    }

    #[cfg(test)]
    fn allows(
        &mut self,
        generation: u64,
        ledger: &aterm_capacity::Ledger,
        machine: &dyn aterm_capacity::Machine,
        need: &aterm_capacity::Need,
        clock: &dyn Fn() -> Instant,
    ) -> bool {
        self.rejection(generation, ledger, machine, need, clock)
            .is_none()
    }

    fn acquired(&mut self) {
        self.generation = None;
        self.sampled = None;
        self.probe = None;
    }
}

fn binary_permit_observed<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    waiters: &BinaryWaiters,
    alone: bool,
    pass: Option<(&Mutex<AdmissionPass>, u64)>,
) -> (
    Option<Result<BinaryPermit<'a>, std::io::Error>>,
    Option<test_waits::Reason>,
) {
    let mut global_wait = None;
    let mut core_wait = None;
    let result = binary_permit_noting(
        ctx,
        pool,
        b,
        &mut |need, mode| {
            if let Some(mode) = mode {
                let result = crate::admit_live(need, mode).map(Some);
                if matches!(&result, Ok(Some(_)))
                    && let Some((pass, _)) = pass
                {
                    pass.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .acquired();
                }
                return result;
            }
            let result = waiters.poll_noting(
                b.index,
                need,
                &aterm_capacity::Live,
                |need| {
                    let ledger = aterm_capacity::Ledger::user().ok_or_else(|| {
                        aterm_capacity::Error::Ledger(std::io::Error::other(
                            "no home directory for binary memory admission",
                        ))
                    })?;
                    aterm_capacity::admit::Waiter::enter(&ledger, &aterm_capacity::Live, need)
                        .map_err(aterm_capacity::Error::Ledger)
                },
                &mut |need| {
                    let (pass, generation) = pass?;
                    let ledger = aterm_capacity::Ledger::user()?;
                    pass.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .rejection(
                            generation,
                            &ledger,
                            &aterm_capacity::Live,
                            need,
                            &Instant::now,
                        )
                },
                &mut global_wait,
            );
            if matches!(&result, Ok(Some(_)))
                && let Some((pass, _)) = pass
            {
                pass.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .acquired();
            }
            result
        },
        alone,
        &mut core_wait,
    );
    (result, core_wait.or(global_wait))
}

#[cfg(test)]
fn binary_permit_with<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    attempt: &mut impl FnMut(
        aterm_capacity::Need,
        Option<aterm_capacity::Admit>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error>,
) -> Option<Result<BinaryPermit<'a>, std::io::Error>> {
    binary_permit_noting(ctx, pool, b, attempt, false, &mut None)
}

fn binary_permit_noting<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    attempt: &mut impl FnMut(
        aterm_capacity::Need,
        Option<aterm_capacity::Admit>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error>,
    alone: bool,
    waiting: &mut Option<test_waits::Reason>,
) -> Option<Result<BinaryPermit<'a>, std::io::Error>> {
    let claim = if let Some(ctx) = ctx.filter(|ctx| ctx.memory.is_some()) {
        let need = aterm_capacity::Need::new("verify test binary")
            .mem(b.peak)
            .cores(if pool.is_some() { 0 } else { b.charge })
            .rank(if ctx.background {
                aterm_capacity::Rank::Background
            } else {
                aterm_capacity::Rank::Ordinary
            })
            .covers();
        let need = if ctx
            .memory
            .as_ref()
            .is_some_and(|book| b.peak >= book.budget())
        {
            need.alone()
        } else {
            need
        };
        // Explicit resource overrides retain their CLI meaning. Background work always keeps
        // its cancellable, non-overriding admission; FIFO tokens remain mandatory either way.
        let mode = (!ctx.background && ctx.quiet == Some(aterm_capacity::Admit::Now))
            .then_some(aterm_capacity::Admit::Now);
        match attempt(need, mode) {
            Ok(Some(claim)) if pool.is_none() && claim.cores() == 0 => {
                *waiting = Some(test_waits::Reason::zero_core_grant());
                return None;
            }
            Ok(Some(claim)) => Some(Arc::new(claim)),
            Ok(None) => return None,
            Err(why) => return Some(Err(std::io::Error::other(why.to_string()))),
        }
    } else {
        None
    };
    let tokens = match pool {
        None => None,
        Some(pool) => {
            let mut tokens = Vec::new();
            for _ in 0..b.charge {
                match pool.try_acquire() {
                    Ok(Some(token)) => tokens.push(token),
                    // LOST TOKENS. `alone` says no binary of this dispatch runs and no child of
                    // this run is alive, so no holder this run knows of is left to give a token
                    // back: a GNU client that died holding one took it with it, and the pool
                    // cannot name that. Waiting for the whole charge would then never end, so
                    // the binary takes the tokens there are and runs within them
                    // ([`BinaryPermit::granted`]). It holds no more than the pool had, so the
                    // pool's other branches are never overdrawn.
                    Ok(None) if alone && !tokens.is_empty() => break,
                    Ok(None) => {
                        *waiting = Some(test_waits::Reason::fifo());
                        return None;
                    }
                    Err(why) => return Some(Err(why)),
                }
            }
            Some(tokens)
        }
    };
    Some(Ok(BinaryPermit { claim, tokens }))
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
#[cfg(test)]
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
#[cfg(test)]
fn admitted_after_stop(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
) -> Option<Planned> {
    admitted_holding(state, turn, live, stop, after_stop, &mut |_| Some(())).map(|(b, ())| b)
}

#[cfg(test)]
fn admitted_holding<T>(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
    acquire: &mut impl FnMut(&Planned) -> Option<T>,
) -> Option<(Planned, T)> {
    admitted_holding_observed(
        state,
        turn,
        live,
        stop,
        after_stop,
        &mut |_, b, _| acquire(b),
        &mut |_, _| {},
    )
}

#[cfg(test)]
fn admitted_holding_pass<T>(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
    acquire: &mut impl FnMut(u64, &Planned) -> Option<T>,
) -> Option<(Planned, T)> {
    admitted_holding_observed(
        state,
        turn,
        live,
        stop,
        after_stop,
        &mut |generation, b, _| acquire(generation, b),
        &mut |_, _| {},
    )
}

fn admitted_holding_observed<T>(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
    acquire: &mut impl FnMut(u64, &Planned, bool) -> Option<T>,
    rejected: &mut impl FnMut(&Planned, test_plan::DispatchResources),
) -> Option<(Planned, T)> {
    loop {
        let stopping = stop() || exec::ending();
        after_stop();
        let mut d = state.lock().unwrap_or_else(PoisonError::into_inner);
        if stopping || stop() {
            d.halt();
        }
        if !d.waiting() {
            return None;
        }
        let mut permit = None;
        let generation = d.generation();
        // Whether no binary of this dispatch runs: `acquire` reads it under this lock.
        let alone = d.held() == 0;
        if let Some(b) = d.next_with_observed(
            live(),
            &mut |b| {
                permit = acquire(generation, b, alone);
                permit.is_some()
            },
            rejected,
        ) {
            return Some((b, permit.expect("admission acquired its resources")));
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

/// What a run carries from prior successful binaries ([`carry`]).
pub struct Carried {
    /// The binaries that run, in their order.
    pub run: Vec<Binary>,
    /// The binaries carried, with current evidence and the prior execution wall.
    pub carried: Vec<CarriedBinary>,
    /// The current evidence of every candidate binary, by spec.
    pub known: BTreeMap<String, Evidence>,
    /// Each binary a bounded prior proof was offered for and which runs again, with the first
    /// thing that differs ([`refusal`]), in the binaries' order.
    pub refused: Vec<(String, String)>,
}

/// Why a bounded prior proof does not carry `evidence`'s binary: a content key that differs
/// names the parts that moved, so a part no two runs can share is read off the first report.
fn refusal(golden: bool, prior: &CarryProof, evidence: &Evidence) -> String {
    if golden {
        return format!("{} always runs", crate::checks::GOLDEN.0);
    }
    if let Some(reason) = &evidence.reason {
        return format!("unbounded here: {reason}");
    }
    if prior.exe_sha256 != evidence.exe_sha256 {
        return "executable bytes changed".into();
    }
    let moved = moved_parts(&prior.key_parts, &evidence.parts);
    if moved.is_empty() {
        "content key changed; the prior proof names no parts".into()
    } else {
        format!("content key moved in {}", moved.join(", "))
    }
}

/// The report's lines for the offered proofs that did not carry: one per reason, most frequent
/// first, each with its count and one binary it holds for.
fn refused_lines(refused: &[(String, String)]) -> Vec<String> {
    let mut reasons: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (spec, why) in refused {
        reasons.entry(why.as_str()).or_default().push(spec.as_str());
    }
    let mut reasons: Vec<_> = reasons.into_iter().collect();
    reasons.sort_by_key(|(_, specs)| std::cmp::Reverse(specs.len()));
    reasons
        .into_iter()
        .map(|(why, specs)| {
            format!(
                "  offered and run again: {} ({why}), as {}",
                specs.len(),
                specs[0]
            )
        })
        .collect()
}

/// A prior success reused without executing this hop.
pub struct CarriedBinary {
    pub binary: Binary,
    pub evidence: Evidence,
    /// The prior run's global libtest JSON decision, which is part of the reconstructed command.
    pub json: bool,
    pub prior_execution_s: f64,
    /// The release run whose execution this is, when its proof names one.
    pub origin_run: Option<String>,
}

/// Which of `bins` a run carries. `evidence` runs for every binary, including a first run
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
    let (mut run, mut carried, mut refused) = (Vec::new(), Vec::new(), Vec::new());
    for (binary, evidence) in bins.into_iter().zip(evidence) {
        let evidence = evidence.expect("every candidate binary was assigned to the evidence pool");
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
                origin_run: prior.expect("matched prior proof").origin_run.clone(),
                binary,
                evidence,
            });
        } else {
            if let Some(prior) = prior.filter(|prior| prior.outcome == "ok" && prior.inputs_bounded)
            {
                refused.push((
                    binary.spec.clone(),
                    refusal(golden(&binary), prior, &evidence),
                ));
            }
            run.push(binary);
        }
    }
    Carried {
        run,
        carried,
        known,
        refused,
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
        crate::event_log::event_value(
            crate::event_log::EventKind::TestBinaryFinished,
            aterm_json::json!({
                  "test": binary.spec,
                "outcome": "ok", "exit_code": aterm_json::Value::Null,
                "waiting_s": 0.0, "execution_s": 0.0,
                "cpu_s": aterm_json::Value::Null, "starved_s": aterm_json::Value::Null,
                "peak_bytes": aterm_json::Value::Null,
                "width": aterm_json::Value::Null, "tests": aterm_json::Value::Null,
                "test_counts": aterm_json::Value::Null,
                "failures": [],
                "detail": format!(
                    "carried{}: prior ok with equal executable and complete runtime inputs",
                    carried
                        .origin_run
                        .as_deref()
                        .map(|run| format!(" from run {run}"))
                        .unwrap_or_default()
                ),
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
                "key_parts": evidence.parts,
                "json": carried.json,
                "recorded_execution_s": carried.prior_execution_s,
                "carried": true,
                "carried_from": carried.origin_run,
            })
        )
    )
}

/// A candidate which matched before dispatch but did not remain equal through the whole run.
/// It never executed, so it is NOT RUN rather than a failure. This later row supersedes any
/// older candidate for the spec and makes the test check incomplete instead of inheriting OK.
fn unstable_carried_event(carried: &CarriedBinary, evidence: &Evidence) -> String {
    let binary = &carried.binary;
    format!(
        "verify-event: {}",
        crate::event_log::event_value(
            crate::event_log::EventKind::TestBinaryFinished,
            aterm_json::json!({
                  "test": binary.spec,
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
        crate::event_log::event_value(
            crate::event_log::EventKind::TestBinaryFinished,
            aterm_json::json!({
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
                "planned_peak_bytes": ran.planned.peak,
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
                "key_parts": evidence.map(|evidence| &evidence.parts),
                "json": json,
                "recorded_execution_s": ran.execution.as_secs_f64(),
                "carried": false,
            })
        )
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
        cores: Some(crate::exec::test_thread_cores(width, ran.planned.charge)),
        tests,
        partition: learned_partition,
        exe_sha256: evidence.and_then(|evidence| evidence.exe_sha256.clone()),
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
/// sample with a zero wall, so the latest FAIL is retained without replacing its last timed
/// wall or changing duration priority.
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

/// The command that runs `b` as cargo would have, with `threads` test threads sharing the `cores`
/// it is charged (`Cmd::test_width`), the run's checker policy and its `Running` header at the
/// head of its log, its cases read as they end ([`exec::CaseWatch`]): in libtest's JSON format,
/// its flags right after the executable, where `watch` says libtest took it and `b` may run with
/// it ([`json_fits`]).
#[must_use]
pub fn binary_cmd(b: &Binary, threads: u32, cores: u32, checkers: bool, watch: Watch) -> Cmd {
    let env: Vec<(OsString, OsString)> = b
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
    let mut command = Cmd::replayed(&b.record.argv, &b.record.cwd, env)
        .test_width(threads, cores)
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
    cores: u32,
    checkers: bool,
    watch: Watch,
) -> Cmd {
    crate::test_env::command(ctx, binary_cmd(binary, threads, cores, checkers, watch))
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

/// A failed recording pass diagnostic. Preserve a bare binary panic location
/// and cause rather than its trailing backtrace hint. This quotes output only:
/// the owned binary exit still supplies the verdict and no case is invented.
fn first_error(log: &str) -> String {
    if let Some(line) = log.lines().find(|l| l.trim_start().starts_with("error")) {
        return line.trim().chars().take(200).collect();
    }
    // A libtest summary already accompanies its separately captured cases. The
    // panic fallback is for bare binaries that supplied no such final summary.
    if let Some(line) = log.lines().rev().find(|line| !line.trim().is_empty())
        && line.trim_start().starts_with("test result:")
    {
        return line.trim().chars().take(200).collect();
    }
    if let Some(at) = log
        .lines()
        .position(|line| line.trim_start().starts_with("thread ") && line.contains(" panicked at "))
    {
        return log
            .lines()
            .skip(at)
            .take(6)
            .map(str::trim)
            .take_while(|line| !line.starts_with("note:") && !line.starts_with("stack backtrace:"))
            .flat_map(|line| line.chars().chain(std::iter::once('\n')))
            .take(200)
            .collect::<String>()
            .trim_end()
            .to_owned();
    }
    log.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .chars()
        .take(200)
        .collect()
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

/// A test run's combined claim in its capacity ledger: memory for its binaries and, outside a
/// release pool, cores for their subprocesses ([`claim_both`]).
pub(crate) struct Claims {
    /// The combined resource claim; none for nothing to claim, or a run with no book.
    pub(crate) resources: Option<crate::ResourceClaim>,
    /// The cores granted: at least one, at most those asked.
    pub(crate) granted: u32,
}

/// Claim `bytes` of memory for `label`'s test binaries (`what` says for what) and `asked` cores
/// together, admitted by `admit`, once the run's claim before (`held`) has gone back. Under a
/// release pool the pool supplies the cores, so this asks only for memory. The atomic unpooled
/// claim cannot hold either resource while waiting for the other.
///
/// # Errors
/// The NOT RUN row's words: why the resources were not claimed
/// ([`crate::claim_test_resources`]), or a grant of no core.
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
    // The release already owns the pool's Release-ranked core claim. Asking the global ledger
    // again would count the build and export branches that spend those same tokens.
    let cores = u32::from(ctx.pool.is_none()).saturating_mul(asked);
    let resources = crate::claim_test_resources(ctx, label, bytes, what, cores, admit)
        .map_err(|why| format!("{label} — {why}"))?;
    let granted = if ctx.pool.is_some() {
        asked
    } else {
        resources
            .as_ref()
            .map_or(asked, |claim| claim.claim.cores().min(asked))
    };
    if granted == 0 {
        return Err("subprocess CPU admission granted no cores; no test was run".to_owned());
    }
    Ok(Claims { resources, granted })
}

/// Record Cargo's current test invocations and run them on the run's CPU share: each binary at
/// the width its kept runs call for and charged the cores they used, as many at once as their
/// charges fit in the share, in recorded-duration and peak-memory priority ([`crate::test_plan`]); and keep
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
    if let Err(why) = crate::test_workers::share(&ctx.env, ctx.pool.as_deref()) {
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
    // heaviest kept peak, or its cold peak, with the run's cores ([`claim_both`]); both are held
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
    /// The selection's heaviest peak, else cold bound, used where a binary has no peak.
    pub(crate) fallback_peak: u64,
    /// The recording pass's memory and core claims, replaced by the dispatch's claims.
    pub(crate) claims: Claims,
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
                granted: share,
            },
            granted_cores: share,
            checkers: false,
            preface: String::new(),
            rerun: crate::findings::Rerun::default(),
        }
    }
}

/// The bound for an unrecorded or unread binary: the selection's heaviest kept peak,
/// else the machine's cold figure. It is positive even if a corrupt machine reading said
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
/// the width, charge, peak, prediction and relative priority from the full-set plan;
/// only cargo-order indexes above the removed binary move down one.
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
    (peak > 0).then_some(peak)
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
    plan: &Plan,
    hang_after: Duration,
) -> Result<Option<Probe>, ProbeHung> {
    let waiters = BinaryWaiters::default();
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
        let planned = plan
            .order
            .iter()
            .find(|p| p.index == index)
            .expect("probe is planned");
        let waits = test_waits::Trace::probe(ctx, bins, index);
        let permit = loop {
            if ctx.halted().is_some() || exec::ending() {
                return Ok(Some(Probe::Unanswered(
                    "run ended before probe admission".into(),
                )));
            }
            // No dispatch runs yet: the probe alone asks, and a listing needs no more than the
            // tokens a pool that lost some still holds.
            let (permit, reason) = binary_permit_observed(
                Some(ctx),
                ctx.pool.as_deref(),
                planned,
                &waiters,
                exec::no_child_runs(),
                None,
            );
            waits.change(index, reason);
            if let Some(permit) = permit {
                match permit {
                    Ok(permit) => break permit,
                    Err(why) => return Ok(Some(Probe::Unanswered(why.to_string()))),
                }
            }
            std::thread::park_timeout(LOOK_AGAIN);
        };
        let _reservation = crate::holding_resource_claim(permit.claim.clone());
        let _cover = crate::memory::covering(
            permit
                .claim
                .as_ref()
                .map(|c| Arc::from(c.record().cover_marker())),
        );
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

/// The salt of a run's test content keys: a release's frozen context (`--carry-context`), or
/// for a development run that brackets its inputs ([`crate::Ctx::input_drift`]) a fixed domain
/// of its own. A development key is compared only within its run, before and after each binary,
/// and no proof leaves it, so one salt for every such run carries nothing between them.
pub(crate) fn evidence_context(ctx: &crate::Ctx) -> Option<String> {
    if ctx.release {
        return ctx.carry_context.clone();
    }
    ctx.input_drift.then(|| {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm-dev-test-input-drift-v1\0");
        aterm_codec::hex::encode_lower(&hash.finalize())
    })
}

/// ` (moved: a, b)` for the key parts two observations of one binary disagree on, or nothing
/// when either recorded none.
fn moved_note(before: &KeyParts, after: &KeyParts) -> String {
    let moved = moved_parts(before, after);
    if moved.is_empty() {
        String::new()
    } else {
        format!(" (moved: {})", moved.join(", "))
    }
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
        granted_cores,
        checkers,
        preface,
        rerun,
    } = planned;
    bins = crate::test_shards::expand(
        bins,
        &history,
        ctx.passed_binaries.as_ref().filter(|_| ctx.release),
    );
    let listed = bins.len();
    // Plan the complete recorded set. A carry may remove work, but cannot change any
    // peer's width or charge after that peer's content key has committed to its command.
    let mut plan = plan_from_history(
        &history,
        &bins,
        granted_cores,
        ctx.test_threads,
        fallback_peak,
    );
    let predicted_peak = plan_claim(&plan).unwrap_or(0);
    r.raw(format!("  test binaries: {} predicted concurrent recorded bytes; reservations are per binary, released at completion; unrecorded bound {}",
        predicted_peak, fallback_peak));
    // Execution claims only admitted binaries, never a whole-stage predicted envelope.
    // Drop recording memory and CPU before any per-binary admission.
    progress.enter(Phase::ExecutionAdmission);
    drop(claims);
    let _background = match dispatch_lifetime(ctx, crate::admit_live) {
        Ok(guard) => guard,
        Err(why) => {
            r.cannot_run(why);
            return;
        }
    };
    progress.enter(Phase::Planning);
    // A failed test reruns at the width its binary runs at, which the full plan sets. A probe hang
    // or carry only projects this plan; neither replans the peers after their widths are fixed.
    let rerun_base = rerun;
    // Local packing obeys the machine's budget too; actual claims arbitrate against slices
    // and other processes at every admission, and return at each binary's completion.
    let memory = ctx.memory.as_ref().map(crate::memory::Book::budget);
    // A candidate is judged before any harness process starts. Reconstruct the prior run's
    // global JSON decision under this hop's already-fixed width, and carry only an equal complete
    // key. Misses finalize their actual capacity cover after admission. The probe sees misses alone, so a
    // binary reported as carried has not even run startup code in this hop.
    let evidenced = AtomicU64::new(0);
    let content_evidence = ctx.release || ctx.input_drift;
    let verifier = crate::report::running_driver().map(str::to_owned);
    let new_evidence_builder = || {
        crate::test_content::EvidenceBuilder::new(
            evidence_context(ctx),
            verifier.clone(),
            ctx.scratch.clone(),
        )
    };
    let candidate_evidence = new_evidence_builder();
    let fixed: BTreeMap<_, _> = plan
        .order
        .iter()
        .map(|planned| (bins[planned.index].spec.clone(), *planned))
        .collect();
    let command_for = |binary: &Binary, json: bool| {
        let planned = fixed
            .get(&binary.spec)
            .expect("every candidate binary is in the fixed test plan");
        let command = final_binary_cmd(
            ctx,
            binary,
            planned.width,
            planned.charge,
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
        let mut evidence = if !ctx.release && !builder.inputs_declared(binary, command) {
            Evidence {
                exe_sha256: None,
                content_key: None,
                reason: Some("runtime inputs have no complete bounded declaration".into()),
                parts: KeyParts::new(),
            }
        } else if exact_error.is_some() {
            let mut incomplete = command.clone();
            incomplete.exact_env = false;
            builder.evidence(binary, &incomplete)
        } else {
            builder.evidence(binary, command)
        };
        if let Some(why) = exact_error {
            evidence.content_key = None;
            evidence.parts.clear();
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
    let (carried, known, refused) = if content_evidence {
        let all = bins;
        let Carried {
            run,
            carried,
            mut known,
            refused,
        } = carry(
            // Only a release carries: a development run's bracket runs every binary.
            ctx.passed_binaries
                .as_ref()
                .filter(|_| ctx.release)
                .unwrap_or(&empty_passed),
            all.clone(),
            |binary, prior| {
                prior.map_or_else(
                    || Evidence {
                        exe_sha256: None,
                        content_key: None,
                        reason: Some("there is no prior successful execution to carry".into()),
                        parts: KeyParts::new(),
                    },
                    |proof| candidate_for(binary, proof.json),
                )
            },
            usize::try_from(granted_cores).unwrap_or(1),
        );
        plan = project_plan(&plan, &all, &run);
        for binary in &run {
            known.remove(&binary.spec);
        }
        bins = run;
        (carried, known, refused)
    } else {
        (Vec::new(), BTreeMap::new(), Vec::new())
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
                        .clone()
                        .or_else(|| candidate.evidence.exe_sha256.clone()),
                    content_key: None,
                    reason: Some(format!(
                        "runtime inputs changed after carry selection; the run-wide read-only \
                         declaration was violated{}",
                        moved_note(&candidate.evidence.parts, &after.parts)
                    )),
                    parts: KeyParts::new(),
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
        match probe_json(ctx, &bins, &plan, probe_hang_after) {
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
                    started: true,
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
                let hung_rerun = rerun_base.clone().at_widths([(
                    binary.spec.clone(),
                    planned.width,
                    planned.charge,
                )]);
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
    let rerun = rerun_base.at_widths(plan.order.iter().map(|planned| {
        (
            bins[planned.index].spec.clone(),
            planned.width,
            planned.charge,
        )
    }));
    if let Some(probe) = &probe {
        r.raw(json_ladder(probe));
    }
    let watch = Watch {
        json: probe == Some(Probe::Took),
        stop_at_red: ctx.halt.armed(),
        seed: ctx.test_seed,
    };
    // Each miss finalizes and retains its exact command under its own admitted cover. The
    // same frozen command brackets both content observations, even after its claim ends.
    let commands: Mutex<BTreeMap<String, (Cmd, Option<String>)>> = Mutex::new(BTreeMap::new());
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
    let ended = |binary: &Binary, ran: &Ran, halt: Option<&dyn Fn()>| {
        if content_evidence && ran.started && ran.run.ok {
            let (command, exact_error) = commands
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&binary.spec)
                .cloned()
                .expect("every successful miss keeps its exact command");
            let after = evidence_for(
                &new_evidence_builder(),
                binary,
                &command,
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
                        reason: Some(format!(
                            "runtime inputs changed while the test binary ran; its read-only \
                             declaration was violated{}",
                            moved_note(&before.parts, &after.parts)
                        )),
                        exe_sha256: after.exe_sha256.or(before.exe_sha256),
                        content_key: None,
                        parts: KeyParts::new(),
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
                        parts: KeyParts::new(),
                    }
                }
            };
            evidence.insert(binary.spec.clone(), stable);
        }
        let evidence = evidence_of(binary);
        let (outcome, event, sample) =
            binary_event(binary, ran, ran.started, evidence.as_ref(), watch.json);
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
    let ran = dispatch_resources(
        &bins,
        &plan,
        memory,
        ctx.pool.as_deref(),
        Some(ctx),
        &live,
        &stop,
        &exec::no_child_runs,
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
            let (cmd, exact_error) = if content_evidence {
                command_for(b, watch.json)
            } else {
                (
                    final_binary_cmd(ctx, b, planned.width, planned.charge, checkers, watch),
                    None,
                )
            };
            if content_evidence {
                commands
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(b.spec.clone(), (cmd.clone(), exact_error.clone()));
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
        |binary, ran, halt| ended(binary, ran, Some(halt)),
    );
    let total = started.elapsed();
    progress.enter(Phase::Reporting);
    // Keep cancellation separate from expiry: a configured ceiling is no evidence it was
    // spent. An unstarted binary keeps the first red's words, or the interrupt's; only an
    // actually spent ceiling is a timeout. Each is heard as the rest were, without a sample.
    let ran: Vec<Option<Ran>> = bins
        .iter()
        .zip(ran)
        .enumerate()
        .map(|(index, (b, ran))| {
            if ran.is_some() {
                return ran;
            }
            let stopped = ctx.halted().or_else(|| {
                exec::ending().then_some("the run was interrupted before this child started")
            });
            let run = match (stopped, ceiling.filter(|limit| total >= *limit)) {
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
                started: false,
                planned,
                waiting: total,
                execution: Duration::ZERO,
            };
            ended(b, &left, None);
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
    // The stage also owns Cargo's recording pass and discovery/probe children. Its CPU is
    // not the sum of dispatched binary rows alone. Preserve that distinction, and an unread
    // or overlapping sample, rather than allocating setup CPU to tests that did not use it.
    let binary = BinaryCpu::of(ran.iter().flatten());
    let stage_ns = crate::memory::current()
        .and_then(|meter| meter.observed_cpu())
        .map(|cpu| cpu.nanoseconds);
    let other_ns = stage_ns.and_then(|stage| stage.checked_sub(binary.ns));
    emit(
        ctx,
        &format!(
            "verify-event: {}",
            crate::event_log::event_value(
                crate::event_log::EventKind::TestCpuAccounted,
                aterm_json::json!({
                    "observed_binary_cpu_s": binary.observed().map(|ns| ns as f64 / 1e9),
                    "binary_reads": binary.reads, "binary_unread": binary.unread,
                    "binary_unstarted": binary.unstarted,
                    "observed_stage_cpu_s": stage_ns.map(|ns| ns as f64 / 1e9),
                    "other_observed_child_cpu_s": other_ns.map(|ns| ns as f64 / 1e9),
                    "reconciled": stage_ns.map(|_| binary.unread == 0 && other_ns.is_some()),
                    "scope": "dispatch binaries plus other observed stage children; sampled lower bounds",
                })
            )
        ),
    );
    // What the binaries' memory claim was, what the run carried and what hashing the
    // executables cost, how the binaries used their charges and whether their history was kept,
    // in a halted run's report as in a whole one's.
    let accounted = |r: &mut Report| {
        r.raw(summary(&plan, &ran));
        if content_evidence {
            r.raw(format!(
                "  test binaries: {} of {listed} carried (prior ok, executable and complete \
                 runtime inputs equal through dispatch); {} run; {} invalidated",
                carried_stable,
                bins.len() + probe_hung_count,
                carried_drift.len()
            ));
            for line in refused_lines(&refused) {
                r.raw(line);
            }
        }
        if content_evidence {
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
            started: true,
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

/// What the test binaries' own clocks read of their trees, for `test-cpu-accounted`: a binary
/// the run left unstarted ([`Ran::started`]) used no CPU and is counted apart, never as a sample
/// lost; a started one whose clock booked no member of its tree is unread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BinaryCpu {
    /// The CPU the read binaries' trees used, nanoseconds.
    ns: u64,
    /// Started binaries whose tree was read.
    reads: u64,
    /// Started binaries with no reading (or one too large to add): unreconciled.
    unread: u64,
    /// Binaries the run left unstarted.
    unstarted: u64,
}

impl BinaryCpu {
    fn of<'a>(ran: impl IntoIterator<Item = &'a Ran>) -> BinaryCpu {
        let mut cpu = BinaryCpu::default();
        for ran in ran {
            if !ran.started {
                cpu.unstarted += 1;
                continue;
            }
            match ran
                .run
                .usage
                .map(|usage| u64::try_from(usage.cpu.as_nanos()))
            {
                Some(Ok(ns)) => {
                    cpu.ns = cpu.ns.saturating_add(ns);
                    cpu.reads += 1;
                }
                Some(Err(_)) | None => cpu.unread += 1,
            }
        }
        cpu
    }

    /// The read binaries' CPU, where it is a figure: some binary was read, or none was unread
    /// (no binary started: zero).
    fn observed(self) -> Option<u64> {
        (self.reads > 0 || self.unread == 0).then_some(self.ns)
    }
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
#[path = "testrun_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "testrun_probe_tests.rs"]
mod probe_tests;

#[cfg(target_os = "macos")]
#[path = "testrun_admission_bench.rs"]
pub(crate) mod admission_bench;
