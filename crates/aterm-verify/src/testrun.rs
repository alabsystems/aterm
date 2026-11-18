// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running test binaries concurrently: each at the width its runs call for, as many at once as
//! their charges fit in the run's CPU share ([`crate::test_plan`]).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::exec::{self, Cmd, Run};
use crate::ladder::{Outcome, Report, Severity};
use crate::test_plan::{self, Dispatcher, Plan, Planned};
use crate::test_progress::{Phase, Progress};

use crate::resource_waits as test_waits;
use aterm_capacity::Machine as _;

/// The flag cargo's runner config hands this binary.
pub const RECORD_FLAG: &str = "--record-test-binary";

/// How long a binary the live-rate backstop holds back waits before it looks again, unless a
/// binary ends first: the stage meter's read cadence, which moves the rate. A dispatch looks
/// once per this interval however many workers it has ([`Dispatcher::look_due`]).
const LOOK_AGAIN: Duration = exec::READ_USAGE_EVERY;

/// How long after a look that met another admission's hold of the ledger's lock the dispatch
/// looks again ([`AdmissionPass::again`]): short, so a binary that fits is not held back for a
/// whole [`LOOK_AGAIN`] behind a peer whose looks keep taking the lock, and bounded, so a lock
/// held for long costs one look of a few touches of the ledger per pause and no more.
const LOCK_AGAIN: Duration = Duration::from_millis(50);

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

/// A new subprocess pool of `slots` stack slots under `scratch`
/// (`aterm_spec::subprocess_lane`'s layout).
pub(crate) fn subprocess_pool(
    scratch: &Path,
    slots: usize,
) -> std::io::Result<aterm_tempfile::TempDir> {
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
    /// first red or the release's pool left unstarted, which used no CPU.
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
/// their last reads. Once `stop()` holds it halts and starts nothing more. `completed` is handed
/// a local halt that takes the same lock as admission, so a decisive red can close dispatch before it writes its event. A
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
/// waits for nobody. The pool's sole adopter counts it first
/// ([`aterm_capacity::Jobserver::count_alone`]) and writes back the tokens no holder has, so the
/// binary takes its whole charge; where that count cannot be judged (another process adopted
/// the pool, its creator is moving an allocation, or a wait of this process is in its read)
/// the binary starts on the tokens there are, at least one, its width and its cores within them
/// (`BinaryPermit::granted`), and a line says so. It holds only what the pool had, so no other
/// holder is overdrawn. Each binary's end in a run is a reap point too: with no binary left
/// running and no child of the run alive, the pool is counted there ([`count_at_reap`]). This
/// dispatch, outside a run, lists no children and waits.
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
        None,
        live,
        stop,
        &|| false,
        run_one,
        completed,
    )
}

/// [`dispatch_pooled`] inside a run (`ctx`), whose binaries each claim their memory and cores
/// as they start ([`binary_permit_in`]), and, where the run has one, on its live share
/// (`share`, [`crate::test_share`]): a binary the plan's share admits beside others starts only
/// while the share as it is at that look covers it too, and waits as one the dispatcher's own
/// rule held back does. One with none beside it asks its claim alone.
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
    share: Option<&crate::test_share::Live>,
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
                // A worker that panics halts the dispatch through its `Charged`, which clears
                // nothing: its retained request leaves the queue here, after that halt.
                let _clear_on_panic = ClearOnPanic(&waiters);
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
                        &mut |look, b, held| {
                            if let Some(share) = share {
                                // One stamp places the dispatch for cores and for memory.
                                share.placed(waiters.place());
                                let rate = live();
                                let (refused, moved) =
                                    share.refuses(b.charge, held, rate, Instant::now());
                                if let (Some(ctx), Some(moved)) = (ctx, moved) {
                                    emit(ctx, &share_event(share.asked(), &moved));
                                }
                                if let Some(refused) = refused {
                                    waits.waits(
                                        b.index,
                                        test_waits::Wait::For(test_waits::Reason::share(
                                            b, held, rate, refused,
                                        )),
                                    );
                                    return None;
                                }
                            }
                            let (permit, wait) = binary_permit_observed(
                                ctx,
                                pool,
                                b,
                                &waiters,
                                &|| held == 0 && idle(),
                                Some((&probes, look)),
                            );
                            if permit.is_some() {
                                waits.started(b.index);
                            } else if let Some(wait) = wait {
                                waits.waits(b.index, wait);
                            }
                            permit
                        },
                        LookHooks {
                            rejected:
                                &mut |b: &Planned, resources: test_plan::DispatchResources| {
                                    waits.waits(
                                        b.index,
                                        test_waits::Wait::For(test_waits::Reason::local(
                                            b, resources,
                                        )),
                                    );
                                },
                            again: &|look| {
                                probes
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .again(look)
                            },
                        },
                    );
                    let Some((planned, permit)) = next else {
                        waits.finish_pending();
                        break;
                    };
                    // What it runs on is what its ledger claim or pool tokens grant.
                    let b = execution_grant(&state, &permit, planned);
                    let source = if pool.is_some() {
                        "release pool"
                    } else {
                        "capacity ledger"
                    };
                    if let (Some(ctx), Some(binary), Ok(permit)) =
                        (ctx, bins.get(b.index), permit.as_ref())
                        && (permit.tokens.is_some() || permit.claim.is_some())
                    {
                        emit(
                            ctx,
                            &core_grant_event(
                                "binary",
                                Some(&binary.spec),
                                (planned.charge, b.charge),
                                source,
                                None,
                                &aterm_capacity::Live.read(None),
                            ),
                        );
                    }
                    if let (true, Some(binary)) = (b.charge < planned.charge, bins.get(b.index)) {
                        eprintln!(
                            "verify: {} starts on {} of its {} requested cores: the {source} \
                             grant bounds its execution width",
                            binary.spec, b.charge, planned.charge
                        );
                    }
                    let charged = Charged {
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
                        // THE PLACE IS HANDED ON BEFORE THE LAST CLAIM GOES ([`BinaryWaiters`]):
                        // where no other binary of the dispatch holds one, the next queued
                        // binary's request is entered at the dispatch's place while this
                        // binary's claim still stands at it, under the dispatcher's mutex, so no
                        // look of another process finds the dispatch gone from the queue.
                        if let Some(ctx) =
                            ctx.filter(|ctx| ctx.memory.is_some() && !overriding(ctx))
                            && permit.as_ref().is_some_and(|p| p.claim.is_some())
                        {
                            let stopping = stop() || exec::ending();
                            let d = state.lock().unwrap_or_else(PoisonError::into_inner);
                            waiters.hand_over(
                                &d,
                                b.index,
                                stopping,
                                &|next| binary_need(ctx, pool.is_some(), next),
                                &aterm_capacity::Live,
                                enter_on_the_ledger,
                            );
                            drop(d);
                        }
                        drop(permit);
                        // For the record of the pool's next count that is not whole.
                        if let (Some(pool), Some(run)) = (pool, answered.as_ref()) {
                            pool.note_reaped(format!(
                                "binary={} code={:?} hung={}",
                                binary.spec,
                                run.code,
                                run.hung.is_some()
                            ));
                        }
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
                    drop(charged);
                    // A reap point: with no binary of this dispatch left running (read here,
                    // never under the count's locks, which a dispatch holding its own lock may
                    // wait on) and no child of the run alive, the pool is counted.
                    if let Some(pool) = pool {
                        let none_runs =
                            state.lock().unwrap_or_else(PoisonError::into_inner).held() == 0;
                        if count_at_reap(pool, &|| none_runs && idle()) {
                            // Tokens put back change no generation: the next look is due now.
                            state
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .look_now();
                            turn.notify_all();
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

/// Count the release's pool at a reap point of this run, a binary's or a stage's end, once
/// `quiet` says nothing of the run is left running (asked again under the count's locks,
/// [`aterm_capacity::Jobserver::count_alone`]): a loss is written back and recorded with the
/// clients noted since the last whole count, and each whole count closes that window, so a
/// later record names only the clients reaped after it. Whether it put tokens back.
pub(crate) fn count_at_reap(pool: &aterm_capacity::Jobserver, quiet: &dyn Fn() -> bool) -> bool {
    if !quiet() {
        return false;
    }
    match pool.count_alone(quiet) {
        Ok(count) => count.reminted() > 0,
        Err(why) => {
            eprintln!("verify: the release's pool was not counted: {why}");
            false
        }
    }
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

/// THE DISPATCH'S PLACE IN THE MEMORY QUEUE, FROM ITS FIRST BINARY TO ITS LAST.
///
/// A dispatch asks the ledger for one claim per binary, and the ledger serves memory first in,
/// first out by when each record was laid. Laid at now, each binary's request went behind
/// every claim any process had laid meanwhile, and a claim that does not fit beside what is
/// ahead spares nothing while it waits and is admitted alone once clear: a dispatch paid one
/// whole trip through the queue per binary, and while other sessions kept laying large claims
/// it did not finish (measured 2026-11-14: five of 298 binaries started in 2 h 40 m).
///
/// So the dispatch keeps ONE PLACE ([`aterm_capacity::admit::Place`]): the order stamp of the
/// first request it entered. Every request it enters after that is laid at that place
/// ([`aterm_capacity::admit::Waiter::enter_at`]): the retained one, which is polled fresh at
/// every look and keeps its record, its claim file and its wait mark until it is admitted or
/// cleared; and the one-shot attempts for the other binaries its share allows, which leave the
/// queue on a shortfall and so reserve nothing lasting, but are judged ahead of the large
/// waiters laid since the place rather than last behind them. And the place is HANDED ON
/// WITHOUT A GAP: while a binary of the dispatch holds its claim the place stands on that
/// claim, and before the last such claim is released the request of the next queued binary is
/// entered at the place and retained ([`Self::hand_over`]), its index chosen under the
/// dispatcher's mutex among the binaries still queued whose need can ever fit
/// ([`aterm_capacity::admit::never`]). A place is good only while a record at it stands, so the
/// dispatch cannot take one it has left: a gap sends its next request to the back, as before.
///
/// Only the LAST claim hands on, so that the request it enters is one the next pass polls: with
/// no binary of the dispatch running, neither the dispatcher's rule nor the live share refuses
/// the head of the plan. Entered while other binaries still ran, a request could stand
/// unpolled for as long as they did (a binary that runs alone, put off until the rest have
/// ended, would hold every later claim back meanwhile, one that fits among them; and its wait
/// mark, never rewritten, would read to other waiters as work that waits, hiding a stalled
/// binary from them).
///
/// This reverses what this type's first comment refused. It still retains one request, never
/// the plan, so one binary's memory is all it reserves; but that one record now holds the head
/// of the queue against later claims for the plan's length. THE COST: a claim laid after the
/// dispatch's first request that cannot fit beside it waits for the dispatch's whole remaining
/// run, not for one binary (and so does what queues behind that claim). It is bounded by the
/// dispatches ahead: a claim laid before the dispatch's first request stays ahead of every
/// binary, and a dispatch that enters after a large waiter stays behind it.
///
/// NO ORPHAN AT THE HEAD. A retained request is always one a pass can poll: its binary is
/// queued (a binary started another way gives it up, [`Self::forget`], and a hand-over drops
/// one whose binary is no longer queued), a halt or a stop clears it ([`Self::clear`], and a
/// worker's panic does, [`ClearOnPanic`]) and a hand-over enters nothing once either holds,
/// and with none of the dispatch's binaries running neither the dispatcher's rule nor the
/// live share refuses the head.
#[derive(Default)]
struct BinaryWaiters {
    state: Mutex<Standing>,
}

/// Clears a dispatch's retained request when its worker unwinds ([`BinaryWaiters::clear`]).
struct ClearOnPanic<'a>(&'a BinaryWaiters);

impl Drop for ClearOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.clear();
        }
    }
}

/// What a dispatch has standing in the ledger's queue.
#[derive(Default)]
struct Standing {
    /// The one retained request: a queued binary's index and its waiter.
    pending: Option<(usize, aterm_capacity::admit::Waiter)>,
    /// The dispatch's place: its first request's, kept by every later one while a record of
    /// the dispatch stands at it.
    place: Option<aterm_capacity::admit::Place>,
    /// The binaries whose claim has reached its hand-over ([`BinaryWaiters::hand_over`]): each
    /// is about to release it, so it no longer keeps the place for the others, though the
    /// dispatcher counts it running until its charge is returned.
    released: std::collections::BTreeSet<usize>,
}

impl Standing {
    /// `waiter` was just entered or is the retained one: its place is the dispatch's from now
    /// on where the dispatch has none that stands (its first request, or the first after it
    /// left the queue). A place that still stands is kept: every entry was made at it.
    fn adopt(&mut self, waiter: &aterm_capacity::admit::Waiter) {
        if self
            .place
            .as_ref()
            .and_then(aterm_capacity::admit::Place::standing_ms)
            .is_none()
        {
            self.place = Some(waiter.place());
        }
    }
}

impl BinaryWaiters {
    fn clear(&self) {
        let pending = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .take();
        drop(pending);
    }

    /// The dispatch's place while it stands, unix ms, and its mark
    /// ([`aterm_capacity::admit::Place::standing`]): the stamp and mark the run's live share
    /// places the dispatch by too ([`crate::test_share::Live::placed`]). `None` before its first
    /// request and once no record of it stands.
    fn place(&self) -> Option<(u64, bool)> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .place
            .as_ref()
            .and_then(aterm_capacity::admit::Place::standing)
    }

    /// Binary `index` started without its retained request (an overriding admission): the
    /// request leaves the queue, so none stands for a binary that will never poll it.
    fn forget(&self, index: usize) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .pending
            .as_ref()
            .is_some_and(|(queued, _)| *queued == index)
        {
            state.pending = None;
        }
    }

    /// BEFORE BINARY `finishing`'S CLAIM IS RELEASED, with the dispatcher's mutex held (`d` is
    /// read under it): where it is the last claim the dispatch holds and no request stands,
    /// enter the next queued binary's at the dispatch's place and retain it, so the queue holds
    /// a record of the dispatch at its place at every instant from its first entry to its last
    /// binary's admission. While another running binary has still to reach its own hand-over,
    /// its claim keeps the place and nothing is entered. The next binary is the first of the
    /// plan's order still queued whose need (`need_of`) can ever fit on `machine`; the next
    /// pass polls it as it polls any retained request. Once the dispatch has halted or is
    /// `stopping` nothing is entered and a retained request is dropped, as [`Self::clear`]
    /// does. Nothing is entered where the dispatch has no place yet. A failed entry leaves
    /// none: the next pass enters as it did before, at now.
    fn hand_over(
        &self,
        d: &Dispatcher,
        finishing: usize,
        stopping: bool,
        need_of: &dyn Fn(&Planned) -> aterm_capacity::Need,
        machine: &dyn aterm_capacity::Machine,
        enter: impl FnOnce(
            aterm_capacity::Need,
            Option<&aterm_capacity::admit::Place>,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.released.insert(finishing);
        if stopping || d.halted() {
            state.pending = None;
            return;
        }
        // A request retained for a binary that is no longer queued would never be polled.
        if state
            .pending
            .as_ref()
            .is_some_and(|(queued, _)| d.phase(*queued) != Some(test_plan::Phase::Queued))
        {
            state.pending = None;
        }
        // Another binary's claim still stands at the place: it hands on, or one after it.
        if d.running().any(|index| !state.released.contains(&index)) {
            return;
        }
        let reading = machine.read(None);
        let next = d
            .queued()
            .map(|b| (b.index, need_of(b)))
            .find(|(_, need)| aterm_capacity::admit::never(need, &reading).is_none());
        if let Some((index, need)) = next {
            Self::stand(&mut state, index, need, enter);
        }
    }

    /// [`Self::hand_over`] for a claim given back inside a pass, before its binary started (the
    /// release's pool had no tokens for it): `index` is that binary, still queued, and the
    /// dispatcher's mutex is the pass's own.
    fn hand_back(
        &self,
        index: usize,
        need: aterm_capacity::Need,
        enter: impl FnOnce(
            aterm_capacity::Need,
            Option<&aterm_capacity::admit::Place>,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        Self::stand(&mut state, index, need, enter);
    }

    /// Enter `need` for binary `index` at the dispatch's place and retain it, where no request
    /// is retained and the dispatch has a place.
    fn stand(
        state: &mut Standing,
        index: usize,
        need: aterm_capacity::Need,
        enter: impl FnOnce(
            aterm_capacity::Need,
            Option<&aterm_capacity::admit::Place>,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
    ) {
        if state.pending.is_some() {
            return;
        }
        let Some(place) = state.place.clone() else {
            return;
        };
        if let Ok(waiter) = enter(need, Some(&place)) {
            state.adopt(&waiter);
            state.pending = Some((index, waiter));
        }
    }

    #[cfg(test)]
    fn poll_with(
        &self,
        index: usize,
        need: aterm_capacity::Need,
        machine: &dyn aterm_capacity::Machine,
        enter: impl FnOnce(
            aterm_capacity::Need,
            Option<&aterm_capacity::admit::Place>,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
        self.poll_noting(index, need, machine, enter, &mut |_, _| None, &mut None)
    }

    /// One look for binary `index`'s `need`. The retained request is polled as it stands; any
    /// other binary's is entered at the dispatch's place (`enter` is handed it; `None` for the
    /// dispatch's first request, which is laid at now and mints the place), polled once, and
    /// retained where it waits and none is, else dropped. While another binary's request is
    /// retained, `rejection` may answer for this one from a shared observation, judged at the
    /// same place, without entering it: a rejection, or the ledger's lock met by that look
    /// ([`aterm_capacity::admit::ProbeRejection::Turn`]), which decides nothing.
    fn poll_noting(
        &self,
        index: usize,
        need: aterm_capacity::Need,
        machine: &dyn aterm_capacity::Machine,
        enter: impl FnOnce(
            aterm_capacity::Need,
            Option<&aterm_capacity::admit::Place>,
        ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>,
        rejection: &mut impl FnMut(
            &aterm_capacity::Need,
            Option<&aterm_capacity::admit::Place>,
        ) -> Option<aterm_capacity::admit::ProbeRejection>,
        waiting: &mut Option<test_waits::Wait>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
        use aterm_capacity::admit::{Polled, ProbeRejection};
        use test_waits::{Reason, Wait};
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let place = state.place.clone();
        // A retained waiter keeps its queue position and always polls fresh. Only temporary
        // alternative envelopes may be rejected by the bounded shared observation.
        if state
            .pending
            .as_ref()
            .is_some_and(|(queued, _)| *queued != index)
            && let Some(rejection) = rejection(&need, place.as_ref())
        {
            *waiting = Some(match rejection {
                ProbeRejection::Waiting(short) => Wait::ledger(&short),
                ProbeRejection::Unreachable(why) => Wait::For(Reason::unreachable(why)),
                ProbeRejection::Turn(holder) => Wait::Unread(holder),
            });
            return Ok(None);
        }
        if let Some(why) = aterm_capacity::admit::never(&need, &machine.read(need.path.as_deref()))
        {
            *waiting = Some(Wait::For(Reason::unreachable(why)));
            return Ok(None);
        }
        let waiter = if state
            .pending
            .as_ref()
            .is_some_and(|(queued, _)| *queued == index)
        {
            state
                .pending
                .take()
                .expect("the queued request was present")
                .1
        } else {
            enter(need, place.as_ref())?
        };
        state.adopt(&waiter);
        match waiter.poll(machine) {
            Polled::Admitted(claim) => Ok(Some(claim)),
            Polled::Waiting(waiter, short) => {
                *waiting = Some(Wait::ledger(&short));
                if state.pending.is_none() {
                    state.pending = Some((index, waiter));
                }
                Ok(None)
            }
            Polled::Gone(why) => Err(aterm_capacity::Error::Ledger(why)),
            Polled::Unqualified(why) => Err(aterm_capacity::Error::Timing(why)),
        }
    }
}

/// Enter `need` in the user's ledger on this machine, at `place` where the dispatch has one.
fn enter_on_the_ledger(
    need: aterm_capacity::Need,
    place: Option<&aterm_capacity::admit::Place>,
) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error> {
    enter_in(
        aterm_capacity::Ledger::user().as_ref(),
        &aterm_capacity::Live,
        need,
        place,
    )
}

/// Enter `need` in `ledger` (`None`: there is no home to keep one under) read on `machine`, at
/// `place` where the dispatch has one.
fn enter_in(
    ledger: Option<&aterm_capacity::Ledger>,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
    place: Option<&aterm_capacity::admit::Place>,
) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error> {
    let ledger = ledger.ok_or_else(|| {
        aterm_capacity::Error::Ledger(std::io::Error::other(
            "no home directory for binary memory admission",
        ))
    })?;
    match place {
        Some(place) => aterm_capacity::admit::Waiter::enter_at(ledger, machine, need, place),
        None => aterm_capacity::admit::Waiter::enter(ledger, machine, need),
    }
    .map_err(aterm_capacity::Error::Ledger)
}

/// WHAT A DISPATCH'S LOOKS SHARE OF THE LEDGER, one look at a time (looks are paced per
/// dispatch, [`Dispatcher::look_due`], and each holds the dispatcher's mutex for its pass).
///
/// The shared observation ([`aterm_capacity::admit::ResourceProbe`]), which rejects the
/// envelopes of binaries other than the retained one without entering them, kept within one
/// generation for at most its age. THE LEDGER'S LOCK, where a try of it met another admission's
/// hold during a look (the observation's own try, the retained request's poll, or a fresh
/// entry's): that is the look's answer for every envelope the observation does not reject, so
/// a look over N binaries touches the ledger a constant number of times, and the next look is
/// due after [`LOCK_AGAIN`]. And the user's ledger, resolved once per look.
#[derive(Default)]
struct AdmissionPass {
    generation: Option<u64>,
    sampled: Option<Instant>,
    probe: Option<Box<aterm_capacity::admit::ResourceProbe>>,
    /// The look that met another admission's hold of the ledger's lock, and that holder.
    turn: Option<(u64, Option<u32>)>,
    /// The ledger the look of that number resolved ([`Self::ledger`]).
    ledger: Option<(u64, Option<aterm_capacity::Ledger>)>,
}

impl AdmissionPass {
    /// What the shared observation says of `need`, entered at `place`, during `look`: a
    /// rejection; the ledger's lock, where the look met it; or `None`, where the need takes a
    /// fresh admission. An observation of another generation answers nothing, a look that met
    /// the lock included; one is taken where the look has none of its generation younger than
    /// its age, and not again in a look that met the lock.
    fn rejection(
        &mut self,
        look: test_plan::Look,
        ledger: &aterm_capacity::Ledger,
        machine: &dyn aterm_capacity::Machine,
        need: &aterm_capacity::Need,
        place: Option<&aterm_capacity::admit::Place>,
        clock: &dyn Fn() -> Instant,
    ) -> Option<aterm_capacity::admit::ProbeRejection> {
        use aterm_capacity::admit::{Capture, ProbeRejection, ResourceProbe};
        let at = clock();
        if self.generation != Some(look.generation) {
            self.generation = Some(look.generation);
            self.sampled = None;
            self.probe = None;
        }
        if self.met(look).is_none()
            && self.sampled.is_none_or(|sampled| {
                at.saturating_duration_since(sampled)
                    >= aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE
            })
        {
            self.sampled = Some(at);
            self.probe = match ResourceProbe::capture_at(ledger, machine, at) {
                Ok(Capture::Observed(probe)) => Some(probe),
                Ok(Capture::Turn(holder)) => {
                    // Nothing was read: the next look tries again, whatever its age.
                    self.sampled = None;
                    self.met_turn(look, holder);
                    None
                }
                Ok(Capture::Unsupported) | Err(_) => None,
            };
        }
        if let Some(rejection) = self
            .probe
            .as_ref()
            .and_then(|probe| probe.rejection_placed(need, clock(), place))
        {
            return Some(rejection);
        }
        self.met(look).map(ProbeRejection::Turn)
    }

    /// The holder of the ledger's lock `look` met, where it met one.
    fn met(&self, look: test_plan::Look) -> Option<Option<u32>> {
        self.turn
            .filter(|(seq, _)| *seq == look.seq)
            .map(|(_, holder)| holder)
    }

    /// `look` met another admission's hold of the ledger's lock, by `holder`.
    fn met_turn(&mut self, look: test_plan::Look, holder: Option<u32>) {
        if self.met(look).is_none() {
            self.turn = Some((look.seq, holder));
        }
    }

    /// How long after `look`'s pass the next look at the same generation is due: [`LOCK_AGAIN`]
    /// where it met the ledger's lock, else [`LOOK_AGAIN`].
    fn again(&self, look: test_plan::Look) -> Duration {
        if self.met(look).is_some() {
            LOCK_AGAIN
        } else {
            LOOK_AGAIN
        }
    }

    /// The user's ledger, resolved once in `look`.
    fn ledger(&mut self, look: test_plan::Look) -> Option<aterm_capacity::Ledger> {
        if self.ledger.as_ref().is_none_or(|(seq, _)| *seq != look.seq) {
            self.ledger = Some((look.seq, aterm_capacity::Ledger::user()));
        }
        self.ledger.as_ref().and_then(|(_, ledger)| ledger.clone())
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
        let look = test_plan::Look { generation, seq: 0 };
        self.rejection(look, ledger, machine, need, None, clock)
            .is_none()
    }

    fn acquired(&mut self) {
        self.generation = None;
        self.sampled = None;
        self.probe = None;
    }
}

/// Enter one request at its retained place, preserving the caller's borrowed lifetime.
type EnterWaiter<'l> = dyn Fn(
        aterm_capacity::Need,
        Option<&aterm_capacity::admit::Place>,
    ) -> Result<aterm_capacity::admit::Waiter, aterm_capacity::Error>
    + 'l;

/// Where a dispatch's binaries ask for their claims: the ledger (the user's, resolved once per
/// look; `None` with no home), the machine its looks read, and how a request is entered there.
struct Asking<'l> {
    ledger: Option<&'l aterm_capacity::Ledger>,
    machine: &'l dyn aterm_capacity::Machine,
    enter: &'l EnterWaiter<'l>,
}

/// [`binary_permit_in`] in the user's ledger on this machine: during a dispatch's `look`, the
/// ledger that look resolved.
fn binary_permit_observed<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    waiters: &BinaryWaiters,
    alone: &dyn Fn() -> bool,
    pass: Option<(&Mutex<AdmissionPass>, test_plan::Look)>,
) -> (
    Option<Result<BinaryPermit<'a>, std::io::Error>>,
    Option<test_waits::Wait>,
) {
    let ledger = match pass {
        Some((pass, look)) => pass
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ledger(look),
        None => aterm_capacity::Ledger::user(),
    };
    let ledger = ledger.as_ref();
    let enter = |need, place: Option<&aterm_capacity::admit::Place>| {
        enter_in(ledger, &aterm_capacity::Live, need, place)
    };
    binary_permit_in(
        ctx,
        pool,
        b,
        waiters,
        alone,
        pass,
        &Asking {
            ledger,
            machine: &aterm_capacity::Live,
            enter: &enter,
        },
    )
}

/// One look's attempt to start binary `b`: its claim asked as [`binary_permit_returning`] asks
/// it, through `waiters` (its retained request polled, any other entered at the dispatch's
/// place) in `asking`'s ledger, with what the look's shared state answers for it
/// ([`AdmissionPass`]). The permit where its resources were acquired, or refused; else what the
/// look found. A look that met the ledger's lock tells `pass`, so it is the look's answer for
/// the other binaries' envelopes.
fn binary_permit_in<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    waiters: &BinaryWaiters,
    alone: &dyn Fn() -> bool,
    pass: Option<(&Mutex<AdmissionPass>, test_plan::Look)>,
    asking: &Asking<'_>,
) -> (
    Option<Result<BinaryPermit<'a>, std::io::Error>>,
    Option<test_waits::Wait>,
) {
    let mut global_wait = None;
    let mut core_wait = None;
    let result = binary_permit_returning(
        ctx,
        pool,
        b,
        &mut |need, mode| {
            if let Some(mode) = mode {
                let result = crate::admit_live(need, mode).map(Some);
                if matches!(&result, Ok(Some(_))) {
                    // It starts without a queued request: none stays behind for it.
                    waiters.forget(b.index);
                    if let Some((pass, _)) = pass {
                        pass.lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .acquired();
                    }
                }
                return result;
            }
            let result = waiters.poll_noting(
                b.index,
                need,
                asking.machine,
                asking.enter,
                &mut |need, place| {
                    let (pass, look) = pass?;
                    pass.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .rejection(
                            look,
                            asking.ledger?,
                            asking.machine,
                            need,
                            place,
                            &Instant::now,
                        )
                },
                &mut global_wait,
            );
            if let Some((pass, look)) = pass {
                let mut pass = pass.lock().unwrap_or_else(PoisonError::into_inner);
                match (&result, &global_wait) {
                    (Ok(Some(_)), _) => pass.acquired(),
                    (Ok(None), Some(test_waits::Wait::Unread(holder))) => {
                        pass.met_turn(look, *holder);
                    }
                    _ => {}
                }
            }
            result
        },
        alone,
        &mut core_wait,
        // The claim goes back with its binary still queued: its request stands again first.
        &mut || {
            if let Some(ctx) = ctx.filter(|ctx| ctx.memory.is_some() && !overriding(ctx)) {
                waiters.hand_back(b.index, binary_need(ctx, pool.is_some(), b), asking.enter);
            }
        },
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
    binary_permit_noting(ctx, pool, b, attempt, &|| false, &mut None)
}

#[cfg(test)]
fn binary_permit_noting<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    attempt: &mut impl FnMut(
        aterm_capacity::Need,
        Option<aterm_capacity::Admit>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error>,
    alone: &dyn Fn() -> bool,
    waiting: &mut Option<test_waits::Wait>,
) -> Option<Result<BinaryPermit<'a>, std::io::Error>> {
    binary_permit_returning(ctx, pool, b, attempt, alone, waiting, &mut || {})
}

/// The claim a binary of the plan asks as it starts: its peak, its charge in cores (none under
/// a release's pool, whose tokens are its cores), at the run's rank, covering its children, and
/// alone where its peak reaches the run's budget.
fn binary_need(ctx: &crate::Ctx, pooled: bool, b: &Planned) -> aterm_capacity::Need {
    let need = aterm_capacity::Need::new(crate::test_share::BINARY_CLAIM)
        .mem(b.peak)
        .cores(if pooled { 0 } else { b.charge })
        .rank(aterm_capacity::Rank::Ordinary)
        .covers();
    if ctx
        .memory
        .as_ref()
        .is_some_and(|book| b.peak >= book.budget())
    {
        need.alone()
    } else {
        need
    }
}

/// Whether the run's binaries are admitted at once whatever the room (`--admit now`): an
/// explicit resource override retains its CLI meaning, and such a start enters no queue.
fn overriding(ctx: &crate::Ctx) -> bool {
    ctx.quiet == Some(aterm_capacity::Admit::Now)
}

/// The claim and tokens binary `b` asks as it starts (`attempt` asks its claim), `waiting` told
/// what a look found where it does not start, calling `giving_back` before a claim it was
/// granted goes back with its binary unstarted (no core in the grant, or no tokens in the
/// release's pool), while that claim is still held.
fn binary_permit_returning<'a>(
    ctx: Option<&crate::Ctx>,
    pool: Option<&'a aterm_capacity::Jobserver>,
    b: &Planned,
    attempt: &mut impl FnMut(
        aterm_capacity::Need,
        Option<aterm_capacity::Admit>,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error>,
    alone: &dyn Fn() -> bool,
    waiting: &mut Option<test_waits::Wait>,
    giving_back: &mut dyn FnMut(),
) -> Option<Result<BinaryPermit<'a>, std::io::Error>> {
    let claim = if let Some(ctx) = ctx.filter(|ctx| ctx.memory.is_some()) {
        let need = binary_need(ctx, pool.is_some(), b);
        // FIFO tokens remain mandatory whatever the admission mode.
        let mode = overriding(ctx).then_some(aterm_capacity::Admit::Now);
        match attempt(need, mode) {
            Ok(Some(claim)) if pool.is_none() && claim.cores() == 0 => {
                *waiting = Some(test_waits::Wait::For(test_waits::Reason::zero_core_grant()));
                giving_back();
                drop(claim);
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
            let mut counted = false;
            while tokens.len() < b.charge as usize {
                match pool.try_acquire() {
                    Ok(Some(token)) => tokens.push(token),
                    Ok(None) => {
                        // LOST TOKENS. `alone` says no binary of this dispatch runs and no child
                        // of this run is alive, so no holder this run knows of is left to give a
                        // token back: a GNU client that died holding one took it with it. The
                        // pool's sole adopter counts it then, with the tokens it holds, against
                        // the allocation published beside it, and makes it whole
                        // ([`aterm_capacity::Jobserver::count_alone`], asking `alone` again
                        // under its locks); the binary then takes its whole charge.
                        let lone = alone();
                        if lone && !std::mem::replace(&mut counted, true) {
                            match pool.count_alone(alone) {
                                Ok(count) if count.reminted() > 0 => continue,
                                Ok(_) => {}
                                Err(why) => {
                                    eprintln!("verify: the release's pool was not counted: {why}");
                                }
                            }
                        }
                        // A count that was not judged (another adopter, a transfer in flight, a
                        // wait in its read) leaves the binary to the tokens there are, at least one, its width
                        // within them ([`BinaryPermit::granted`]): no more than the pool had,
                        // so no other holder is overdrawn.
                        if lone && !tokens.is_empty() {
                            break;
                        }
                        *waiting = Some(test_waits::Wait::For(test_waits::Reason::fifo()));
                        if claim.is_some() {
                            giving_back();
                        }
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
        LookHooks {
            rejected: &mut |_: &Planned, _: test_plan::DispatchResources| {},
            again: &|_| LOOK_AGAIN,
        },
    )
}

#[cfg(test)]
fn admitted_holding_pass<T>(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
    acquire: &mut impl FnMut(test_plan::Look, &Planned) -> Option<T>,
) -> Option<(Planned, T)> {
    admitted_holding_observed(
        state,
        turn,
        live,
        stop,
        after_stop,
        &mut |look, b, _| acquire(look, b),
        LookHooks {
            rejected: &mut |_: &Planned, _: test_plan::DispatchResources| {},
            again: &|_| LOOK_AGAIN,
        },
    )
}

/// Rejection notification and next-look pacing for one dispatch look.
struct LookHooks<'a, R> {
    rejected: &'a mut R,
    again: &'a dyn Fn(test_plan::Look) -> Duration,
}

/// The next binary a look of the dispatch admits, `None` once none is left to start or the run
/// halted. Each worker checks the stop and the halt first; then, where a look is due
/// ([`Dispatcher::look_due`]), passes over the queue under the dispatcher's mutex, `acquire`
/// asked of each binary the dispatcher's rule lets start (`rejected` told of each it holds
/// back) with the look's identity and the cores the running binaries hold; a pass that starts
/// nothing is stamped when it ends, the next look at that generation due `again(look)` after
/// it. A worker whose look is not due waits until it is, or until a binary starts or ends.
fn admitted_holding_observed<T, R>(
    state: &Mutex<Dispatcher>,
    turn: &Condvar,
    live: &(dyn Fn() -> f64 + Sync),
    stop: &(dyn Fn() -> bool + Sync),
    after_stop: &(impl Fn() + Sync),
    acquire: &mut impl FnMut(test_plan::Look, &Planned, u32) -> Option<T>,
    hooks: LookHooks<'_, R>,
) -> Option<(Planned, T)>
where
    R: FnMut(&Planned, test_plan::DispatchResources),
{
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
        // ONE LOOK PER DISPATCH PER INTERVAL: a worker whose look another worker has just made
        // waits out what is left of its interval, and looks at once after a generation change.
        let wait = if let Some(left) = d.look_due(Instant::now()) {
            left
        } else {
            let look = d.begin_look();
            let mut permit = None;
            // The cores this dispatch's running binaries hold, none where none runs: `acquire`
            // reads it under this lock.
            let held = d.held();
            if let Some(b) = d.next_with_observed(
                live(),
                &mut |b| {
                    permit = acquire(look, b, held);
                    permit.is_some()
                },
                hooks.rejected,
            ) {
                return Some((b, permit.expect("admission acquired its resources")));
            }
            let again = (hooks.again)(look);
            d.looked(look, Instant::now(), again);
            again
        };
        drop(match turn.wait_timeout(d, wait) {
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
/// takes ([`measured`]; `started`: the dispatcher admitted it): its outcome, its waits, its
/// tree's usage (`null` where unread), the test threads it ran at (`width`) and the cores of the
/// share it was charged (`charge`, [`crate::test_plan`]), and the tests libtest said it runs.
fn binary_event(
    binary: &Binary,
    ran: &Ran,
    started: bool,
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
                "json": json,
                "recorded_execution_s": ran.execution.as_secs_f64(),
            })
        )
    );
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let stopped =
        started && (run.stopped_at.is_some() || run.hung.is_some()) && run.spawn_error.is_none();
    let sample = (measured(run, started) || stopped).then(|| crate::test_history::Sample {
        at_ms: aterm_time::unix_millis().unwrap_or(0),
        outcome: crate::report::word(outcome).to_owned(),
        wall_ms: if stopped { 0 } else { wall_ms(ran.execution) },
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
/// `started`, nothing outside it ended it (no
/// [`Run::spawn_error`]: a spawn refused, an interrupt that came before it started, a stuck
/// holder it waited behind, a wait for it that failed, or an end unrun at macOS's program check,
/// `exec::start`: a program its syspolicyd stranded, or at its ceiling a leader not yet started or
/// a member held past a first exec's bound) and no interrupt was ending the run as it ended
/// ([`crate::exec::ending`]), which cuts a binary short at a point that says nothing of it. A
/// binary its own work ceiling ended keeps its sample, a lower bound of what it takes, unless verify
/// ended it unrun there; one that never started would keep a wall of about 0 ms, and push a
/// measured sample out of its row. A binary verify stopped at its first red case
/// ([`Run::stopped_at`]) is deliberately not measured: its caller keeps an outcome-only schema-1
/// sample with a zero wall, so the latest FAIL is retained without replacing its last timed
/// wall or changing duration priority.
/// A measured run's own wall in whole milliseconds, rounded up: a binary that ran in under a
/// millisecond still took time, and a wall of 0 is a stopped run's, which plans nothing
/// (`test_history::Sample::timed`).
fn wall_ms(execution: Duration) -> u64 {
    u64::try_from(execution.as_micros().div_ceil(1000))
        .unwrap_or(u64::MAX)
        .max(1)
}

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
/// alone has no [`crate::libtest::RAN_NO_TEST`] row). With no binary at all, `decide` reads the
/// empty log.
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

/// A test run's two core counts: what it asked, and what its recording pass was granted.
///
/// THE GRANT IS ONE LOOK AT THE LEDGER, taken as the run's own test compile ends: an ordinary
/// run's compile claims memory and no cores, so the 5-minute load it left reads as work nobody
/// claimed and the ledger shares the machine with it (12 cores at a load of 24 grant 4). It
/// bounds the recording pass, and it is the share the binaries' widths, charges and peaks are
/// planned on ([`Cores::planned_on`]): the widths their kept runs measured them at. How many of
/// them run side by side follows the run's share as it is at each start
/// ([`crate::test_share`]), within the cores asked ([`Cores::dispatch_share`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cores {
    /// The share asked: [`crate::Ctx::test_share`], which `--test-cores` caps.
    pub(crate) asked: u32,
    /// What the recording pass's claim was granted ([`Claims::granted`]).
    pub(crate) granted: u32,
}

impl Cores {
    /// The share each binary's width, charge and peak are planned on ([`plan_from_history`]):
    /// the recording pass's grant, at least one.
    pub(crate) const fn planned_on(self) -> u32 {
        if self.granted == 0 { 1 } else { self.granted }
    }

    /// The most cores the dispatch's running binaries hold together
    /// ([`test_plan::Plan::share`]): the cores asked, at least one.
    pub(crate) const fn dispatch_share(self) -> u32 {
        if self.asked == 0 { 1 } else { self.asked }
    }

    /// The multi-process test stacks the run's pool is made for
    /// ([`subprocess_slots_for_cores`]): those of the cores asked. A run on a live share holds
    /// back the slots that share does not cover ([`crate::test_share::Slots`]).
    pub(crate) const fn subprocess_slots(self) -> usize {
        subprocess_slots_for_cores(self.dispatch_share())
    }

    /// Whether the recording pass was granted less than the run asked.
    pub(crate) const fn held_back(self) -> bool {
        self.granted < self.asked
    }
}

/// The plan a run dispatches: each binary's width, charge and peak planned on the recording
/// pass's grant ([`Cores::planned_on`]), dispatched within the cores the run asked
/// ([`Cores::dispatch_share`], [`test_plan::Plan::within`]), the binaries of `scope`'s seeds
/// first ([`plan_from_history`]).
fn dispatch_plan(
    history: &crate::test_history::History,
    bins: &[Binary],
    scope: &crate::scope::Scope,
    cores: Cores,
    ceiling: Option<u32>,
    fallback_peak: u64,
) -> Plan {
    plan_from_history(
        history,
        bins,
        scope,
        cores.planned_on(),
        ceiling,
        fallback_peak,
    )
    .within(cores.dispatch_share())
}

/// Keep recorded binary budgets while allowing a release's returned credits to pack peers.
/// Recording can observe a smaller allocation while the fixture producer holds a token. The
/// FIFO still admits only whole charges it actually holds, including before that token returns.
fn pool_dispatch_plan(
    plan: Plan,
    pool: Option<&aterm_capacity::Jobserver>,
    test_cores: Option<u32>,
) -> Plan {
    let Some(pool) = pool else {
        return plan;
    };
    let grant = pool.grant().max(1);
    let ceiling = test_cores.map_or(grant, |cap| cap.clamp(1, grant));
    plan.within(ceiling)
}

/// The `test-core-grant` event: what one claim of cores for the test run asked and was granted,
/// and the machine's load as it was granted. `scope` is `recording` for the recording pass's
/// claim ([`claim_both`]), with the subprocess slots open under it, or `binary` for one
/// binary's own claim as it starts, with its rerun spec; `source` names who granted it. An
/// observation only: it says why a grant was what it was (the ledger shares by the 5-minute
/// load, [`aterm_capacity`]'s `grant_iter`), and decides nothing.
fn core_grant_event(
    scope: &str,
    test: Option<&str>,
    (asked, granted): (u32, u32),
    source: &str,
    subprocess_slots: Option<usize>,
    machine: &aterm_capacity::Reading,
) -> String {
    format!(
        "verify-event: {}",
        crate::event_log::event_value(
            crate::event_log::EventKind::TestCoreGrant,
            aterm_json::json!({
                "scope": scope, "test": test, "asked": asked, "granted": granted,
                "source": source, "subprocess_slots": subprocess_slots,
                "load1": machine.load1, "load5": machine.load5,
                "machine_cores": machine.cores,
            })
        )
    )
}

/// The `test-core-grant` event of scope `dispatch`: the run's live share
/// ([`crate::test_share::Live`]) at a look that moved it, of the cores asked, with what the look
/// read: the load, the cores other claims hold and the load they account for, the waiters
/// ahead, whether the run holds the judgment turn (its share then all the room), the load this
/// run's own binaries put in, and the subprocess slots left open. Its
/// `source` is `recording grant` where the ledger could not be read.
fn share_event(asked: u32, standing: &crate::test_share::Standing) -> String {
    let look = standing.look.as_ref();
    format!(
        "verify-event: {}",
        crate::event_log::event_value(
            crate::event_log::EventKind::TestCoreGrant,
            aterm_json::json!({
                "scope": "dispatch", "test": None::<&str>, "asked": asked,
                "granted": standing.share,
                "source": if look.is_some() { "live share" } else { "recording grant" },
                "subprocess_slots": standing.slots.map(|open| open[0]),
                "load1": look.and_then(|l| l.machine.load1),
                "load5": look.and_then(|l| l.machine.load5),
                "machine_cores": look.map(|l| l.machine.cores),
                "held_by_others": look.map(|l| l.others.held),
                "others_in_load": look.map(|l| l.others.in_load),
                "waiting_ahead": look.map(|l| l.others.waiting),
                "holds_turn": look.map(|l| l.others.turn),
                "own_load": standing.own_load,
            })
        )
    )
}

/// Record Cargo's current test invocations and run them on the run's CPU share: each binary at
/// the width its kept runs call for and charged the cores they used, as many at once as their
/// charges fit in the share, in recorded-duration and peak-memory priority ([`crate::test_plan`]); and keep
/// what each took for the next run ([`crate::test_history`]). A run whose binaries claim their
/// cores in the ledger reads that share again at each start ([`crate::test_share`]), within the
/// cores it asked ([`Cores`]). Each binary that runs libtest's
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
    let cores = Cores {
        asked: asked_cores,
        granted: claims.granted,
    };
    let recording_resources = claims.resources.as_ref();
    // The pin of the recording pass, which runs under the recording claim.
    let thread_override = (ctx.env.build_slot_held.is_some()
        || ctx.pool.is_some()
        || ctx.test_threads.is_some()
        || ctx.test_cores.is_some()
        || cores.held_back())
    .then(|| ctx.test_threads_within(cores.granted));
    let slots = match subprocess_pool(&ctx.scratch, cores.subprocess_slots()) {
        Ok(pool) => pool,
        Err(why) => {
            r.cannot_run(format!(
                "cannot initialize shared subprocess pool: {why}; no test was run"
            ));
            return;
        }
    };
    // A run whose binaries claim their cores in the ledger runs them on its live share. A
    // release's pool is its share already, and a person's `--now` asked for no waiting.
    let share = (recording_resources.is_some()
        && ctx.pool.is_none()
        && ctx.quiet != Some(aterm_capacity::Admit::Now))
    .then(|| {
        crate::test_share::Live::on_the_ledger(
            cores.asked,
            cores.granted,
            aterm_capacity::Rank::Ordinary,
            Some(crate::test_share::Slots::of(
                slots.path(),
                cores.subprocess_slots(),
            )),
        )
    });
    emit(
        ctx,
        &core_grant_event(
            "recording",
            None,
            (cores.asked, cores.granted),
            match (&ctx.pool, recording_resources) {
                (Some(_), _) => "release pool",
                (None, Some(_)) => "capacity ledger",
                (None, None) => "unclaimed",
            },
            Some(if share.is_some() {
                subprocess_slots_for_cores(cores.granted)
            } else {
                cores.subprocess_slots()
            }),
            &aterm_capacity::Live.read(None),
        ),
    );
    if share.is_some() {
        r.raw(format!(
            "  test cores: the recording pass was granted {} of the {} asked; the binaries are \
             planned on {}, and as many run at once as the run's share covers at each start, \
             at most {}",
            cores.granted,
            cores.asked,
            cores.planned_on(),
            cores.dispatch_share()
        ));
    }

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
    let recording = crate::admission_rows::Recording {
        claim: claims
            .resources
            .as_ref()
            .map(|held| held.claim.record().need.mem),
        peak: pass.usage.map(|usage| usage.peak_bytes),
    };
    let planned = RunPlan {
        bins,
        history,
        fallback_peak,
        recording,
        claims,
        cores,
        share,
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
    /// What the recording pass claimed and held, for the run's admission-log row.
    pub(crate) recording: crate::admission_rows::Recording,
    /// The recording pass's memory and core claims, which go back before the dispatch: each
    /// binary then claims its own.
    pub(crate) claims: Claims,
    /// What the run asked and its recording pass was granted: the full binary set is planned on
    /// the grant ([`dispatch_plan`]); pooled packing may use later returned credits without
    /// replanning those binary budgets ([`pool_dispatch_plan`]).
    pub(crate) cores: Cores,
    /// The run's live share ([`crate::test_share`]), where its binaries claim their cores in
    /// the ledger: none under a release's pool, with no capacity book, or under `--now`.
    pub(crate) share: Option<crate::test_share::Live>,
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
            recording: crate::admission_rows::Recording::default(),
            claims: Claims {
                resources: None,
                granted: share,
            },
            cores: Cores {
                asked: share,
                granted: share,
            },
            share: None,
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
/// positive memory cost until this machine has measured one for it. The binaries of a crate the
/// change itself touched ([`crate::scope::Scope::touches`]: a cone's seeds; every crate of any
/// other scope) lead the plan ([`test_plan::plan`]).
fn plan_from_history(
    history: &crate::test_history::History,
    bins: &[Binary],
    scope: &crate::scope::Scope,
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
    let seeds: Vec<bool> = bins
        .iter()
        .map(|b| crate::scope::Scope::package_of(&b.spec).is_some_and(|name| scope.touches(name)))
        .collect();
    test_plan::plan(&seen, &seeds, share, ceiling, fallback_peak)
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

/// Each binary's rerun spec with the width and the charge it ran at ([`Ran::planned`]): what
/// its ledger claim or pool tokens granted where that was less than the plan
/// ([`execution_grant`]). A binary the run left unstarted has the plan's.
fn ran_widths(bins: &[Binary], plan: &Plan, ran: &[Option<Ran>]) -> Vec<(String, u32, u32)> {
    plan.order
        .iter()
        .filter_map(|planned| {
            let at = ran
                .get(planned.index)
                .and_then(Option::as_ref)
                .filter(|ran| ran.started)
                .map_or(*planned, |ran| ran.planned);
            Some((bins.get(planned.index)?.spec.clone(), at.width, at.charge))
        })
        .collect()
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
            let (permit, wait) = binary_permit_observed(
                Some(ctx),
                ctx.pool.as_deref(),
                planned,
                &waiters,
                &exec::no_child_runs,
                None,
            );
            if let Some(permit) = permit {
                waits.started(index);
                match permit {
                    Ok(permit) => break permit,
                    Err(why) => return Ok(Some(Probe::Unanswered(why.to_string()))),
                }
            }
            // A look that met only the ledger's lock is made again soon ([`LOCK_AGAIN`]).
            let again = if matches!(wait, Some(test_waits::Wait::Unread(_))) {
                LOCK_AGAIN
            } else {
                LOOK_AGAIN
            };
            if let Some(wait) = wait {
                waits.waits(index, wait);
            }
            std::thread::park_timeout(again);
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
        recording,
        claims,
        cores,
        share,
        checkers,
        preface,
        rerun,
    } = planned;
    bins = crate::test_shards::expand(bins, &history);
    // Plan the complete recorded set.
    let mut plan = pool_dispatch_plan(
        dispatch_plan(
            &history,
            &bins,
            &ctx.scope,
            cores,
            ctx.test_threads,
            fallback_peak,
        ),
        ctx.pool.as_deref(),
        ctx.test_cores,
    );
    let predicted_peak = plan_claim(&plan).unwrap_or(0);
    r.raw(format!("  test binaries: {} predicted concurrent recorded bytes; reservations are per binary, released at completion; unrecorded bound {}",
        predicted_peak, fallback_peak));
    // Execution claims only admitted binaries, never a whole-stage predicted envelope.
    // Drop recording memory and CPU before any per-binary admission.
    progress.enter(Phase::ExecutionAdmission);
    drop(claims);
    progress.enter(Phase::Planning);
    // A failed test reruns at the width its binary runs at, which the full plan sets. A probe hang
    // only projects this plan; it does not replan the peers after their widths are fixed.
    let rerun_base = rerun;
    // Local packing obeys the machine's budget too; actual claims arbitrate against slices
    // and other processes at every admission, and return at each binary's completion.
    let memory = ctx.memory.as_ref().map(crate::memory::Book::budget);
    let mut preface = preface;
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
                let (outcome, event, sample) = binary_event(&binary, &ran, true, true);
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
    if let Some(probe) = &probe {
        r.raw(json_ladder(probe));
    }
    let watch = Watch {
        json: probe == Some(Probe::Took),
        stop_at_red: ctx.halt.armed(),
        seed: ctx.test_seed,
    };
    let meter = crate::memory::current();
    let live = || meter.as_ref().and_then(|m| m.live_rate()).unwrap_or(0.0);
    progress.enter(Phase::Dispatch);
    let started = Instant::now();
    let stop = || ctx.halted().is_some();
    let dispatch_samples = Mutex::new(Vec::with_capacity(bins.len()));
    // Hear one binary's end: classify its red before event I/O can hold up the local stop, emit
    // its FAIL before the halt's NOT RUN events, arm that run-wide halt, and keep its sample.
    let ended = |binary: &Binary, ran: &Ran, halt: Option<&dyn Fn()>| {
        let (outcome, event, sample) = binary_event(binary, ran, ran.started, watch.json);
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
        share.as_ref(),
        &live,
        &stop,
        &exec::no_child_runs,
        |b, planned, local_halt| {
            if ctx.halted().is_some() {
                return None;
            }
            // Each admitted binary starts its own work clock inside exec::run_test.
            let cmd = final_binary_cmd(ctx, b, planned.width, planned.charge, checkers, watch);
            let mut run = exec::run_test(&cmd, ctx.exec_env(), local_halt);
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
    // A halt accounts for its queued binaries. An unexplained omission is a gate defect.
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
            let run = match stopped {
                Some(words) => Run {
                    case_observations: None,
                    ok: false,
                    output: String::new(),
                    code: None,
                    spawn_error: Some(words.to_owned()),
                    ..Run::default()
                },
                None => never_run(),
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
    crate::admission_rows::tests(ctx, label, &bins, &ran, &history, recording);
    ctx.tests_cpu_ms
        .fetch_add(binary.ns / 1_000_000, std::sync::atomic::Ordering::Relaxed);
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
    // What the binaries' memory claim was, how the binaries used their charges and whether their
    // history was kept, in a halted run's report as in a whole one's.
    // A failed test reruns at the width its binary ran at and the grant it ran under.
    let rerun = rerun_base.at_widths(ran_widths(&bins, &plan, &ran));
    let accounted = |r: &mut Report| {
        r.raw(summary(&plan, &ran));
        if let Some((least, most)) = share.as_ref().and_then(crate::test_share::Live::range) {
            r.raw(format!(
                "  test cores: the run's share was {least} to {most} of the {} asked, read at \
                 each start",
                plan.share
            ));
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
        &[],
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

/// A binary the dispatcher omitted without a halt: a defect of this gate, and
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
