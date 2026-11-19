// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closing compile can describe the native test launches it just built.
//! An export belongs only to this verifier invocation. A compiler without the export
//! or a capture that cannot be reconciled uses the ordinary Cargo recording pass.

#[path = "test_launch_directory.rs"]
mod directory;
use crate::compiler_artifact::Artifact;
use crate::exec::{self, Cmd};
use crate::testrun::{Binary, Record, TestExe};
use crate::{Ctx, test_index::Index};
use directory::{Directory, HeldDirectory};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const SCHEMA: &str = "targo-test-launches-v1";
const BASE: &str = "targo-test-launch-base-v1 ";
const EXPORT: &str = "--emit-test-launches";
const LIMIT: u64 = 8 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    reason: String,
    schema: String,
    available: bool,
    path: PathBuf,
    sha256: String,
    size: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    available: bool,
    unavailable: Option<String>,
    host: String,
    target_directory: Option<PathBuf>,
    runtime_directories: Vec<Directory>,
    invocation: Vec<String>,
    no_fail_fast: bool,
    base_environment_sha256: String,
    launches: Vec<Launch>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    package_id: String,
    manifest_path: PathBuf,
    target: String,
    kind: String,
    harness: bool,
    tested: bool,
    executable: PathBuf,
    program: PathBuf,
    sha256: String,
    size: u64,
    cwd: PathBuf,
    args: Vec<String>,
    runtime_search_paths: Vec<PathBuf>,
    environment: BTreeMap<String, Option<String>>,
}

fn digest_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

// These values designate a compiler wrapper, proof producer or live scheduling owner.
// A Targo environment edit may clear one, but only the local dispatcher can grant it.
fn runtime_value_allowed(key: &str) -> bool {
    !matches!(
        key,
        "RUSTC"
            | "RUSTDOC"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "RUSTFLAGS"
            | "RUSTDOCFLAGS"
            | "CARGO_ENCODED_RUSTFLAGS"
            | "CARGO_ENCODED_RUSTDOCFLAGS"
            | "CARGO_BUILD_RUSTFLAGS"
            | "CARGO_BUILD_RUSTDOCFLAGS"
            | "CARGO_BUILD_RUSTC"
            | "CARGO_BUILD_RUSTDOC"
            | "CARGO_BUILD_RUSTC_WRAPPER"
            | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
            | "MAKEFLAGS"
            | "MFLAGS"
            | "CARGO_MAKEFLAGS"
            | "TARGO_MAKEFLAGS"
            | "TARGO_JOBSERVER_HANDOFF"
            | "BUILD_SLOT_HELD"
            | "ATERM_CAPACITY_COVER"
            | "ATERM_DISK_COVERED"
            | "ATERM_RELEASE_POOL"
            | "ATERM_IMPLICIT_POOL"
            | "ATERM_RELEASE_RUN"
            | "ATERM_WAITS"
    ) && !key.starts_with("ATERM_VERIFY_UNIT_")
        && (!(key.starts_with("TRUST_") || key.starts_with("TRUSTD_")) || key == "TRUST_STAGE2_BIN")
}

/// Ask the very frontend that made the compile for the base it would use now.
/// Its stdout contains only the digest. An older Targo rejects the option.
fn base(ctx: &Ctx, cmd: &Cmd) -> Option<String> {
    let mut query = cmd.clone();
    query.args = ["--unverified", "test", "--test-launch-base-digest"]
        .map(OsString::from)
        .into();
    let exec::MachineRun { run, stdout } = exec::run_machine(&query, ctx.exec_env(), None);
    if !run.ok {
        return None;
    }
    let output = stdout.ok()?;
    let text = output.trim_end_matches(['\r', '\n']).strip_prefix(BASE)?;
    digest_hex(text).then(|| text.to_owned())
}

#[derive(PartialEq, Eq)]
struct RunSelection {
    program: PathBuf,
    cwd: Option<PathBuf>,
    args: Vec<OsString>,
}

impl RunSelection {
    fn of(command: &Cmd) -> Self {
        Self {
            program: command.program.clone(),
            cwd: command.cwd.clone(),
            args: command.args.clone(),
        }
    }

    fn is_exact_selected_compile(&self, compile: &Cmd) -> bool {
        compile.program == self.program
            && compile.cwd == self.cwd
            && matches!(
                compile.args.strip_prefix(self.args.as_slice()),
                Some([no_run, messages])
                    if no_run == "--no-run" && messages == "--message-format=json-render-diagnostics"
            )
    }
}

pub(crate) struct Request {
    directory: aterm_tempfile::TempDir,
    slots: aterm_tempfile::TempDir,
    slot_count: usize,
    command: Cmd,
    host: String,
    tools: Vec<(PathBuf, Artifact)>,
    invocation: Vec<String>,
    run_selection: RunSelection,
    target_directory: PathBuf,
    measuring: bool,
    compiler_environment: Vec<(OsString, OsString)>,
}

impl Request {
    pub(crate) fn for_compile(ctx: &Ctx, compile: &Cmd, run: &Cmd) -> Option<Self> {
        let run_selection = RunSelection::of(run);
        let eligible = if ctx.release {
            ctx.pool.is_some() && ctx.scope.is_workspace()
        } else {
            !ctx.scope.is_workspace() && run_selection.is_exact_selected_compile(compile)
        };
        if !eligible
            || ctx.compile_only
            || !ctx.checks.contains(&crate::Check::Test)
            || !run.exact_env
            || !compile.exact_env
        {
            return None;
        }
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return None;
        }
        base(ctx, compile)?;
        aterm_uds::host::open_file_room(4096).ok()?;
        let tools = [&ctx.tools.targo, &ctx.tools.targo.with_file_name("trustc")]
            .into_iter()
            .map(|path| {
                let selected = path.canonicalize().ok()?;
                Some((path.clone(), Artifact::retain(&selected).ok()?))
            })
            .collect::<Option<Vec<_>>>()?;
        let host = crate::cells::host_triple(ctx).ok()?;
        let target_directory = ctx.target_dir.canonicalize().ok()?;
        let cores = ctx.test_share();
        let slot_count = crate::testrun::subprocess_slots_for_cores(cores);
        let slots = crate::testrun::subprocess_pool(&ctx.scratch, slot_count).ok()?;
        let directory = aterm_tempfile::TempDir::new_in(&ctx.scratch).ok()?;
        let mut command = crate::compile_account::with_message_format(compile.clone());
        for (key, value) in &run.envs {
            command = command.env(key, value);
        }
        if let Some(threads) = ctx.test_thread_override() {
            command = command.env("RUST_TEST_THREADS", threads.to_string());
        }
        command = command.env(crate::testrun::SUBPROCESS_SLOTS_ENV, slots.path());
        let mut invocation = command
            .args
            .iter()
            .map(|arg| arg.to_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        if invocation
            .first()
            .is_some_and(|word| word == "--unverified")
        {
            invocation.remove(0);
        }
        // These compiler controls can affect the producer's base digest. Only a
        // later metadata-only query receives them; no test inherits a compiler lease.
        let compiler_environment =
            exec::compiler_metadata_environment(&command, ctx.exec_env()).ok()?;
        command = command
            .arg(EXPORT)
            .arg(directory.path().join("launches.json"));
        Some(Self {
            directory,
            slots,
            slot_count,
            command,
            host,
            tools,
            invocation,
            run_selection,
            target_directory,
            measuring: crate::plan::folds_measuring(ctx),
            compiler_environment,
        })
    }

    pub(crate) fn command(&self) -> &Cmd {
        &self.command
    }

    fn manifest(&self, stdout: &str) -> Result<Manifest, String> {
        let mut receipts = Vec::new();
        let mut finished = 0;
        for line in stdout.lines() {
            let Ok(value) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            match value.get("reason").and_then(aterm_json::Value::as_str) {
                Some("test-launches") => {
                    receipts.push(aterm_json::from_str::<Receipt>(line).map_err(|e| e.to_string())?)
                }
                Some("build-finished") if value["success"].as_bool() == Some(true) => finished += 1,
                Some("build-finished") => return Err("Targo reported an incomplete compile".into()),
                _ => {}
            }
        }
        let [receipt] = receipts.as_slice() else {
            return Err("expected one Targo launch receipt".into());
        };
        let path = self.directory.path().join("launches.json");
        if finished != 1
            || receipt.reason != "test-launches"
            || receipt.schema != SCHEMA
            || receipt.path != path
            || receipt.size > LIMIT
            || !digest_hex(&receipt.sha256)
        {
            return Err("launch receipt differs from the closing compile".into());
        }
        let (bytes, _) =
            aterm_uds::atomic_file::read_regular(&path, LIMIT).map_err(|e| e.to_string())?;
        let mut hash = aterm_digest::Sha256::new();
        hash.update(&bytes);
        if bytes.len() as u64 != receipt.size
            || aterm_codec::hex::encode_lower(&hash.finalize()) != receipt.sha256
        {
            return Err("launch manifest differs from Targo's receipt".into());
        }
        let manifest: Manifest = aterm_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if manifest.schema != SCHEMA
            || manifest.available != receipt.available
            || manifest.host != self.host
            || !manifest.no_fail_fast
            || manifest.launches.len() > 4096
            || !digest_hex(&manifest.base_environment_sha256)
        {
            return Err("launch manifest differs from the requested test compile".into());
        }
        if !manifest.available || manifest.unavailable.is_some() {
            return Err("Targo could not describe all native launches".into());
        }
        if manifest.invocation != self.invocation {
            return Err("Targo compiled a different invocation".into());
        }
        Ok(manifest)
    }
}

/// Retain each original description as the existing compiler index reads its digest.
/// A stricter retention failure leaves ordinary indexing and discovery available.
pub(super) struct Capture {
    request: Request,
    manifest: Manifest,
    held: BTreeMap<PathBuf, Artifact>,
    failure: Option<String>,
}

impl Capture {
    pub(super) fn new(request: Request, stdout: &str) -> Result<Self, String> {
        let manifest = request.manifest(stdout)?;
        Ok(Self {
            request,
            manifest,
            held: BTreeMap::new(),
            failure: None,
        })
    }

    pub(super) fn digest(&mut self, path: &Path) -> std::io::Result<String> {
        if self.failure.is_none() {
            match Artifact::retain(path) {
                Ok(original) => {
                    let digest = original.sha256();
                    if self.held.insert(path.to_owned(), original).is_some() {
                        self.failure = Some("duplicate compiler executable".into());
                    }
                    return Ok(digest);
                }
                Err(why) => {
                    self.failure = Some(why);
                    self.held.clear();
                }
            }
        }
        crate::test_index::digest(path)
    }
}

pub(crate) struct Prepared {
    identity: (String, String),
    tools: Vec<(PathBuf, Artifact)>,
    base: String,
    run_selection: RunSelection,
    target_directory: PathBuf,
    compiler_environment: Vec<(OsString, OsString)>,
    slots: Option<aterm_tempfile::TempDir>,
    pub(super) slot_count: usize,
    tests: Vec<(Launch, TestExe)>,
    artifacts: BTreeMap<PathBuf, Artifact>,
    directories: BTreeMap<PathBuf, HeldDirectory>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("selected_tests", &self.tests.len())
            .field("held_output_count", &self.artifacts.len())
            .finish_non_exhaustive()
    }
}

impl Prepared {
    pub(crate) fn capture(
        capture: Capture,
        index: &Index,
        tests: &[TestExe],
    ) -> Result<Self, String> {
        let Capture {
            request,
            manifest,
            mut held,
            failure,
        } = capture;
        if let Some(why) = failure {
            return Err(why);
        }
        if index.binaries.len() != tests.len() || tests.len() != manifest.launches.len() {
            return Err("launches and compiler test artifacts have different counts".into());
        }
        if manifest.target_directory.as_ref() != Some(&request.target_directory) {
            return Err("Targo compiled in a different native target directory".into());
        }
        if manifest.runtime_directories.len() > 128
            || manifest
                .runtime_directories
                .iter()
                .map(Directory::entries)
                .sum::<usize>()
                > 65536
        {
            return Err("runtime directory membership exceeds the supported limit".into());
        }
        let mut libraries_bytes = 0_u64;
        for directory in &manifest.runtime_directories {
            libraries_bytes = libraries_bytes
                .checked_add(directory.library_bytes()?)
                .ok_or("runtime library size overflow")?;
        }
        if libraries_bytes > 512 << 20 {
            return Err("runtime libraries exceed byte limit".into());
        }
        let mut directories = BTreeMap::new();
        for directory in manifest.runtime_directories {
            let held = HeldDirectory::capture(directory)?;
            if directories.insert(held.path().to_owned(), held).is_some() {
                return Err("duplicate library search directory".into());
            }
        }
        let mut used_directories = BTreeSet::new();
        let mut artifacts = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        for launch in manifest.launches {
            for dir in &launch.runtime_search_paths {
                if !directories.contains_key(dir) {
                    return Err("unbound native library search path".into());
                }
                used_directories.insert(dir.clone());
            }
            let found = tests
                .iter()
                .find(|t| {
                    held.get(&t.exe)
                        .is_some_and(|a| a.canonical == launch.executable)
                })
                .ok_or("launch has no matching compiler test artifact")?;
            let entry = index
                .binaries
                .iter()
                .find(|t| {
                    t.path == found.exe
                        && t.package == launch.package_id
                        && t.manifest_path == launch.manifest_path
                        && t.name == launch.target
                })
                .ok_or("launch differs from the compiler's package or target")?;
            let kind = if found.flag == "--lib" {
                "lib"
            } else {
                entry.kind.as_str()
            };
            if !seen.insert(launch.executable.clone())
                || !launch.program.is_absolute()
                || launch.program.canonicalize().ok().as_ref() != Some(&launch.executable)
                || launch.kind != kind
                || found.harness != launch.harness
                || !launch.args.is_empty()
                || entry.sha256 != launch.sha256
                || launch.cwd.canonicalize().ok()
                    != launch
                        .manifest_path
                        .parent()
                        .and_then(|p| p.canonicalize().ok())
            {
                return Err("ambiguous or changed test launch".into());
            }
            // The closing compile selects --tests plus explicitly named measuring
            // benches. Cargo selects --tests by Target::tested(), including opted-in
            // examples and benches. Only the extra measured targets can fall outside it.
            if !launch.tested {
                if !request.measuring
                    || kind != "bench"
                    || !crate::stages::MEASURING_TARGETS.contains(&launch.target.as_str())
                {
                    return Err("unexpected artifact outside the ordinary test selection".into());
                }

                continue;
            }
            let artifact = held
                .remove(&entry.path)
                .ok_or("compiler executable has no original open file")?;
            artifact.validate_identity()?;
            if artifact.sha256() != launch.sha256 || artifact.len() != launch.size {
                return Err("original test executable changed before it could be retained".into());
            }
            artifacts.insert(entry.path.clone(), artifact);
            for (_, path) in &found.bin_env {
                let path = Path::new(path);
                if !artifacts.contains_key(path) {
                    artifacts.insert(path.to_owned(), Artifact::retain(path)?);
                }
            }
            selected.push((launch, found.clone()));
        }
        if used_directories.len() != directories.len() {
            return Err("unselected native library search path".into());
        }
        Ok(Self {
            directories,
            identity: (index.tree.clone(), index.toolchain.clone()),
            base: manifest.base_environment_sha256,
            run_selection: request.run_selection,
            target_directory: request.target_directory,
            tools: request.tools,
            compiler_environment: request.compiler_environment,
            slots: Some(request.slots),
            slot_count: request.slot_count,
            tests: selected,
            artifacts,
        })
    }

    pub(crate) fn take_slots(&mut self) -> Option<aterm_tempfile::TempDir> {
        self.slots.take()
    }

    pub(crate) fn check_context(&self, ctx: &Ctx, serial: &Cmd) -> Result<(), String> {
        if self.run_selection != RunSelection::of(serial) {
            return Err("requested native test command changed".into());
        }
        if ctx.target_dir.canonicalize().ok().as_ref() != Some(&self.target_directory) {
            return Err("native target directory changed".into());
        }
        for (selected, artifact) in &self.tools {
            if selected.canonicalize().ok().as_ref() != Some(&artifact.canonical) {
                return Err("selected toolchain changed".into());
            }
            artifact.validate_identity()?;
        }
        if crate::test_index::identities(&ctx.root, &ctx.tools.targo)
            .ok()
            .as_ref()
            != Some(&self.identity)
        {
            return Err("test source tree or compiler changed".into());
        }
        let mut query = serial.clone();
        for (key, value) in &self.compiler_environment {
            query = query.env(key, value);
        }
        if base(ctx, &query).as_deref() != Some(self.base.as_str()) {
            return Err("Targo runtime base changed".into());
        }
        Ok(())
    }

    pub(crate) fn binaries(&self, serial: &Cmd) -> Result<Vec<Binary>, String> {
        let mut bins = Vec::new();
        for (launch, target) in &self.tests {
            let mut env: BTreeMap<OsString, OsString> = serial.envs.iter().cloned().collect();
            for (name, value) in &launch.environment {
                if name.is_empty()
                    || name.contains(['=', '\0'])
                    || value
                        .as_ref()
                        .is_some_and(|v| v.contains('\0') || !runtime_value_allowed(name))
                {
                    return Err("invalid runtime environment edit".into());
                }
                if let Some(value) = value {
                    env.insert(name.into(), value.into());
                } else {
                    env.remove(OsStr::new(name));
                }
            }
            let loader = if cfg!(target_os = "macos") {
                "DYLD_FALLBACK_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            };
            if !launch.runtime_search_paths.is_empty()
                && !env.get(OsStr::new(loader)).is_some_and(|value| {
                    std::env::split_paths(value)
                        .take(launch.runtime_search_paths.len())
                        .eq(launch.runtime_search_paths.iter().cloned())
                })
            {
                return Err("native search directories differ from the runtime command".into());
            }
            for (name, value) in &launch.environment {
                if name.starts_with("CARGO_BIN_EXE_")
                    && let Some(value) = value
                    && !target
                        .bin_env
                        .iter()
                        .any(|(k, v)| k == name.as_str() && v == value.as_str())
                {
                    return Err("unmatched package companion in runtime environment".into());
                }
            }
            env.extend(target.bin_env.iter().cloned());
            let package = env
                .get(OsStr::new("CARGO_PKG_NAME"))
                .and_then(|v| v.to_str())
                .filter(|v| !v.is_empty())
                .ok_or("Targo omitted the package name from a test launch")?;
            let spec = format!("-p {package} {}", target.flag);
            let header = format!(
                "     Running {} ({})",
                launch.target,
                launch.executable.display()
            );
            bins.push(Binary {
                header,
                spec,
                harness: target.harness,
                record: Record {
                    cwd: launch.cwd.clone(),
                    argv: vec![launch.program.clone().into_os_string()],
                    env: env.into_iter().collect(),
                },
            });
        }
        Ok(bins)
    }

    /// The index has already read each digest from this very descriptor.
    pub(crate) fn validate_originals(&self) -> Result<(), String> {
        for (launch, _) in &self.tests {
            if launch.program.canonicalize().ok().as_ref() != Some(&launch.executable) {
                return Err("selected test program changed".into());
            }
        }
        for directory in self.directories.values() {
            directory.validate()?;
        }
        for artifact in self.artifacts.values() {
            artifact.validate_identity()?;
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_originals()?;
        for artifact in self.artifacts.values() {
            artifact.validate()?;
        }
        Ok(())
    }
}

/// Check the named executable and its companion binaries while the binary has its
/// actual permit. A changed path or held file makes the result unusable even if
/// the child's own exit status was successful.
pub(crate) fn guard(
    prepared: Option<&Prepared>,
    binary: &Binary,
    run: impl FnOnce() -> exec::Run,
) -> exec::Run {
    let Some(prepared) = prepared else {
        return run();
    };
    let check = || {
        let exe = binary.record.exe();
        let (launch, target) = prepared
            .tests
            .iter()
            .find(|(launch, _)| launch.program == exe)
            .ok_or_else(|| format!("test launch has no held executable for {}", exe.display()))?;
        if exe.canonicalize().ok().as_ref() != Some(&launch.executable) {
            return Err("native test program changed".into());
        }
        for path in &launch.runtime_search_paths {
            prepared
                .directories
                .get(path)
                .ok_or("missing held runtime search directory")?
                .validate_selected()?;
        }
        for path in std::iter::once(target.exe.as_path())
            .chain(target.bin_env.iter().map(|(_, value)| Path::new(value)))
        {
            prepared
                .artifacts
                .get(path)
                .ok_or("missing held compiler artifact")?
                .validate_identity()?;
        }
        Ok::<_, String>(())
    };
    if let Err(why) = check() {
        return exec::Run {
            spawn_error: Some(why),
            ..exec::Run::default()
        };
    }
    let mut actual = run();
    if let Err(why) = check() {
        actual.ok = false;
        actual.spawn_error = Some(why);
    }
    actual
}

#[cfg(test)]
#[path = "test_launch_tests.rs"]
mod tests;
