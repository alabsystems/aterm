// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content evidence for carrying one release test binary across candidate hops.
//!
//! A package opts into bounded reuse with `[package.metadata.aterm.test-inputs]` and
//! `bounded = true`. `paths` declares package-relative runtime roots and `workspace-paths`
//! declares workspace-relative roots. `content-only = true` is the audited assertion that the
//! target observes those roots only through path names, entry types and regular-file bytes, and
//! opens no other file or external tool; metadata identity, timestamps, ownership, ACLs, xattrs,
//! interpreters and dynamic tool inputs are therefore out of this schema and fail closed.
//! `read-only = true` additionally asserts that the target, every peer in the release dispatch
//! and external writers leave its roots, executable, helpers and toolchain inputs immutable,
//! including transient mutate-and-restore writes which snapshots cannot observe. The
//! finalized environment is exact. `volatile-environment` is the closed set of per-invocation
//! verifier variables whose concrete values the target reads; it must be present even when the
//! set is empty. An empty root list asserts that the tests open no files. Compiled sources are
//! deliberately not roots: their effect is already in the executable digest, and a `#[cfg(test)]`
//! edit which leaves another binary byte-equal must not invalidate it. Every unknown field,
//! absent declaration, symlink, or path outside its base is unbounded.
//! Execution, working-directory, manifest and helper paths are absolute. The command must resolve
//! to its recorded executable. Working-directory and executable alias spellings stay in the key,
//! together with their resolved targets, so a symlink cannot silently redirect either identity.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::exec::Cmd;
use crate::testrun::Binary;

/// The on-disk proof and content-key domain.
pub const SCHEMA: u32 = 1;

/// One prior successful execution. A `--passed-binaries` file is JSONL, one proof per line.
///
/// `outcome` deliberately remains in the proof even though only `ok` is accepted: a release
/// cannot accidentally feed a prior FAIL into the carry set by changing only its selector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarryProof {
    pub schema: u32,
    pub spec: String,
    pub outcome: String,
    pub exe_sha256: Option<String>,
    pub content_key: Option<String>,
    pub inputs_bounded: bool,
    pub input_reason: Option<String>,
    /// Whether the run's libtest probe selected JSON for test invocations.  A carried binary
    /// reconstructs this exact prior invocation before any current-hop harness is started.
    pub json: bool,
    pub execution_s: f64,
}

/// Prior successful executions, by Cargo rerun spec.
pub type Passed = BTreeMap<String, CarryProof>;

/// The current bytes and runtime-input identity of one binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub exe_sha256: Option<String>,
    pub content_key: Option<String>,
    pub reason: Option<String>,
}

impl Evidence {
    #[must_use]
    pub fn bounded(&self) -> bool {
        self.exe_sha256.is_some() && self.content_key.is_some() && self.reason.is_none()
    }
}

impl CarryProof {
    /// A proof for a binary which actually passed in this invocation.
    #[must_use]
    pub fn passed(spec: impl Into<String>, evidence: &Evidence, execution_s: f64) -> Self {
        Self {
            schema: SCHEMA,
            spec: spec.into(),
            outcome: "ok".into(),
            exe_sha256: evidence.exe_sha256.clone(),
            content_key: evidence.content_key.clone(),
            inputs_bounded: evidence.bounded(),
            input_reason: evidence.reason.clone(),
            json: false,
            execution_s,
        }
    }

    fn validate(&self, line: usize) -> Result<(), String> {
        let digest = |value: &str| aterm_codec::hex::is_lower(value, 64);
        if self.schema != SCHEMA {
            return Err(format!(
                "line {line} has proof schema {}, expected {SCHEMA}",
                self.schema
            ));
        }
        if self.spec.trim().is_empty() || self.spec != self.spec.trim() {
            return Err(format!("line {line} has an empty or untrimmed test spec"));
        }
        if self.outcome != "ok" {
            return Err(format!(
                "line {line} has outcome {:?}; only a prior ok may be carried",
                self.outcome
            ));
        }
        if !self.execution_s.is_finite() || self.execution_s < 0.0 {
            return Err(format!("line {line} has an invalid execution_s"));
        }
        if self
            .exe_sha256
            .as_deref()
            .is_some_and(|value| !digest(value))
        {
            return Err(format!("line {line} has an invalid executable SHA-256"));
        }
        if self
            .content_key
            .as_deref()
            .is_some_and(|value| !digest(value))
        {
            return Err(format!("line {line} has an invalid content key"));
        }
        if self.inputs_bounded {
            if self.exe_sha256.is_none()
                || self.content_key.is_none()
                || self.input_reason.is_some()
            {
                return Err(format!(
                    "line {line} says inputs are bounded without both digests, or with a reason"
                ));
            }
        } else if self.content_key.is_some()
            || self
                .input_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(format!(
                "line {line} says inputs are unbounded without exactly one nonempty reason"
            ));
        }
        Ok(())
    }
}

/// Read the versioned JSONL proof file.
///
/// # Errors
/// A line is not a schema-1 proof, is not a prior `ok`, contradicts its boundedness, has an
/// invalid digest/wall, or repeats a spec. The obsolete `<sha256> <spec>` format is rejected.
pub fn read_passed(text: &str) -> Result<Passed, String> {
    let mut passed = Passed::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = index + 1;
        let proof: CarryProof = aterm_json::from_str(line)
            .map_err(|error| format!("line {line_number} is not a carry proof: {error}"))?;
        proof.validate(line_number)?;
        if passed.insert(proof.spec.clone(), proof).is_some() {
            return Err(format!("line {line_number} repeats a test spec"));
        }
    }
    Ok(passed)
}

/// Encode proofs in spec order as versioned JSONL.
///
/// # Errors
/// A caller constructed a proof which [`read_passed`] would refuse, or JSON serialization fails.
pub fn encode_passed(passed: &Passed) -> Result<String, String> {
    let mut text = String::new();
    for proof in passed.values() {
        proof.validate(text.lines().count() + 1)?;
        text.push_str(&aterm_json::to_string(proof).map_err(|error| error.to_string())?);
        text.push('\n');
    }
    Ok(text)
}

#[derive(Clone, Debug)]
struct PackageInputs {
    digest: String,
    volatile_environment: Vec<String>,
}

/// Shared caches for a release's parallel evidence workers. Package trees are read once per
/// package and helper bytes once per physical executable.
#[derive(Debug)]
pub struct EvidenceBuilder {
    carry_context: Option<String>,
    verifier_sha256: Option<String>,
    scratch: PathBuf,
    packages: Mutex<BTreeMap<(PathBuf, String), Result<PackageInputs, String>>>,
    files: Mutex<BTreeMap<PathBuf, Result<String, String>>>,
}

impl EvidenceBuilder {
    #[must_use]
    pub fn new(
        carry_context: Option<String>,
        verifier_sha256: Option<String>,
        scratch: PathBuf,
    ) -> Self {
        Self {
            carry_context,
            verifier_sha256,
            scratch,
            packages: Mutex::new(BTreeMap::new()),
            files: Mutex::new(BTreeMap::new()),
        }
    }

    /// Evidence for `binary`. The executable is always hashed first, even when the package's
    /// runtime inputs are unbounded, so every release binary has byte identity on its first run.
    #[must_use]
    pub fn evidence(&self, binary: &Binary, command: &Cmd) -> Evidence {
        let exe_sha256 = self.file_digest(&command.program).ok();
        let content_key = self.content_key(binary, command, exe_sha256.as_deref());
        match content_key {
            Ok(content_key) => Evidence {
                exe_sha256,
                content_key: Some(content_key),
                reason: None,
            },
            Err(reason) => Evidence {
                exe_sha256,
                content_key: None,
                reason: Some(reason),
            },
        }
    }

    fn content_key(
        &self,
        binary: &Binary,
        command: &Cmd,
        exe_sha256: Option<&str>,
    ) -> Result<String, String> {
        if !command.program.is_absolute() || !binary.record.exe().is_absolute() {
            return Err(format!(
                "the command and recorded executable paths must be absolute: {} and {}",
                command.program.display(),
                binary.record.exe().display()
            ));
        }
        if !same_physical_path(&command.program, binary.record.exe())? {
            return Err(format!(
                "the test command executes {}, not its recorded executable {}",
                command.program.display(),
                binary.record.exe().display()
            ));
        }
        let cwd = command
            .cwd
            .as_deref()
            .ok_or_else(|| "the test command has no exact working directory".to_owned())?;
        if !cwd.is_absolute() {
            return Err(format!(
                "the test command's working directory is not absolute: {}",
                cwd.display()
            ));
        }
        let physical_cwd = cwd.canonicalize().map_err(|error| {
            format!(
                "the test command's working directory {} cannot be resolved: {error}",
                cwd.display()
            )
        })?;
        if !physical_cwd.is_dir() {
            return Err(format!(
                "the test command's working directory {} is not a directory",
                cwd.display()
            ));
        }
        let exe_sha256 = exe_sha256.ok_or_else(|| {
            format!(
                "the executable cannot be read: {}",
                command.program.display()
            )
        })?;
        let context = self
            .carry_context
            .as_deref()
            .filter(|value| aterm_codec::hex::is_lower(value, 64))
            .ok_or_else(|| "the release supplied no valid carry context".to_owned())?;
        let verifier = self
            .verifier_sha256
            .as_deref()
            .filter(|value| aterm_codec::hex::is_lower(value, 64))
            .ok_or_else(|| "the running verifier cannot be hashed".to_owned())?;
        if !command.exact_env || !command.runtime_environment_is_finalized() {
            return Err(
                "the test command does not declare its finalized complete environment".to_owned(),
            );
        }
        let exact_env = effective_env(command);
        let manifest = exact_env
            .get(b"CARGO_MANIFEST_DIR".as_slice())
            .map(|value| PathBuf::from(os_from_bytes(value)))
            .ok_or_else(|| "CARGO_MANIFEST_DIR is not declared for this binary".to_owned())?;
        if !manifest.is_absolute() {
            return Err(format!(
                "CARGO_MANIFEST_DIR is not absolute: {}",
                manifest.display()
            ));
        }
        let package = self.package_inputs(&manifest, &binary.spec)?;
        let env = content_environment(&exact_env, &self.scratch, &package.volatile_environment);

        let mut hash = aterm_digest::Sha256::new();
        part(&mut hash, b"domain", b"aterm-test-content-key-v1");
        part(&mut hash, b"spec", binary.spec.as_bytes());
        part(&mut hash, b"executable", exe_sha256.as_bytes());
        part(
            &mut hash,
            b"executable-runtime-identity",
            &runtime_file_identity(&command.program)?,
        );
        part(
            &mut hash,
            b"working-directory-resolved",
            &os_bytes(physical_cwd.as_os_str()),
        );
        part(&mut hash, b"package-inputs", package.digest.as_bytes());
        part(&mut hash, b"carry-context", context.as_bytes());
        part(&mut hash, b"verifier", verifier.as_bytes());
        command_identity(&mut hash, command, &env);

        for (name, path) in env
            .iter()
            .filter(|(name, _)| name.starts_with(b"CARGO_BIN_EXE_"))
        {
            let path = PathBuf::from(os_from_bytes(path));
            if !path.is_absolute() {
                return Err(format!(
                    "{} names a non-absolute helper {}",
                    String::from_utf8_lossy(name),
                    path.display()
                ));
            }
            let digest = self.file_digest(&path).map_err(|why| {
                format!(
                    "{} names an unreadable helper {}: {why}",
                    String::from_utf8_lossy(name),
                    path.display()
                )
            })?;
            part(&mut hash, b"helper-name", name);
            part(&mut hash, b"helper-bytes", digest.as_bytes());
            part(
                &mut hash,
                b"helper-runtime-identity",
                &runtime_file_identity(&path)?,
            );
        }

        Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
    }

    fn package_inputs(&self, manifest: &Path, spec: &str) -> Result<PackageInputs, String> {
        let target = target_key(spec)?;
        let key = (manifest.to_path_buf(), target.clone());
        if let Some(cached) = self
            .packages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned()
        {
            return cached;
        }
        let computed = package_inputs(manifest, &target);
        self.packages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, computed.clone());
        computed
    }

    fn file_digest(&self, path: &Path) -> Result<String, String> {
        let key = path.to_path_buf();
        if let Some(cached) = self
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned()
        {
            return cached;
        }
        let computed = digest_file(path);
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, computed.clone());
        computed
    }
}

fn package_inputs(manifest_dir: &Path, target: &str) -> Result<PackageInputs, String> {
    let package = manifest_dir
        .canonicalize()
        .map_err(|error| format!("package root {}: {error}", manifest_dir.display()))?;
    let manifest = package.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let document: aterm_toml::Value =
        aterm_toml::from_str(&text).map_err(|error| format!("{}: {error}", manifest.display()))?;
    let workspace = workspace_root(&package)?;
    if !package.starts_with(&workspace) {
        return Err(format!(
            "package root {} escapes workspace {}",
            package.display(),
            workspace.display()
        ));
    }
    let table = document
        .get("package")
        .and_then(|value| value.get("metadata"))
        .and_then(|value| value.get("aterm"))
        .and_then(|value| value.get("test-inputs"))
        .and_then(aterm_toml::Value::as_table)
        .ok_or_else(|| {
            "package metadata does not declare [package.metadata.aterm.test-inputs] bounded = true"
                .to_owned()
        })?;
    const FIELDS: [&str; 7] = [
        "bounded",
        "content-only",
        "read-only",
        "paths",
        "workspace-paths",
        "volatile-environment",
        "targets",
    ];
    const TARGET_FIELDS: [&str; 6] = [
        "bounded",
        "content-only",
        "read-only",
        "paths",
        "workspace-paths",
        "volatile-environment",
    ];
    if let Some(field) = table.keys().find(|field| !FIELDS.contains(&field.as_str())) {
        return Err(format!("test-inputs has unknown field {field:?}"));
    }
    let targets = match table.get("targets") {
        None => None,
        Some(value) => Some(
            value
                .as_table()
                .ok_or_else(|| "test-inputs.targets is not a table".to_owned())?,
        ),
    };
    if let Some(targets) = targets {
        for (name, value) in targets {
            target_key_from_metadata(name)?;
            let target_table = value
                .as_table()
                .ok_or_else(|| format!("test-inputs.targets.{name:?} is not a table"))?;
            if let Some(field) = target_table
                .keys()
                .find(|field| !TARGET_FIELDS.contains(&field.as_str()))
            {
                return Err(format!(
                    "test-inputs.targets.{name:?} has unknown field {field:?}"
                ));
            }
        }
    }
    let selected = targets.and_then(|targets| targets.get(target));
    let setting = |name: &str| {
        selected
            .and_then(|target| target.get(name))
            .or_else(|| table.get(name))
    };
    if setting("bounded").and_then(aterm_toml::Value::as_bool) != Some(true) {
        return Err(format!(
            "test-inputs target {target:?} does not declare bounded = true"
        ));
    }
    if setting("content-only").and_then(aterm_toml::Value::as_bool) != Some(true) {
        return Err(format!(
            "test-inputs target {target:?} does not declare content-only = true"
        ));
    }
    if setting("read-only").and_then(aterm_toml::Value::as_bool) != Some(true) {
        return Err(format!(
            "test-inputs target {target:?} does not declare read-only = true"
        ));
    }
    if setting("volatile-environment").is_none() {
        return Err(format!(
            "test-inputs target {target:?} does not declare its volatile environment read set"
        ));
    }
    let list = |name: &str| -> Result<Vec<String>, String> {
        match setting(name) {
            None => Ok(Vec::new()),
            Some(value) => value
                .as_array()
                .ok_or_else(|| format!("test-inputs.{name} is not an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| format!("test-inputs.{name} has a non-string or empty item"))
                })
                .collect(),
        }
    };
    let mut roots = Vec::new();
    for relative in list("paths")? {
        roots.push((b"package-runtime".as_slice(), relative, package.clone()));
    }
    for relative in list("workspace-paths")? {
        let path = declared_path(&workspace, &relative)?;
        if !path.starts_with(&package) {
            return Err(format!(
                "workspace runtime input {} is outside package {}; no reader-to-cone mapping declares it",
                path.display(),
                package.display()
            ));
        }
        roots.push((b"workspace-runtime".as_slice(), relative, workspace.clone()));
    }
    let mut volatile_environment = list("volatile-environment")?;
    let allowed = [
        crate::runs::VERIFY_RUN_ENV,
        crate::testrun::SUBPROCESS_SLOTS_ENV,
        aterm_capacity::COVER_ENV,
        "HOME",
        "USERPROFILE",
        "TMPDIR",
    ];
    for name in &volatile_environment {
        if !allowed.contains(&name.as_str()) {
            return Err(format!(
                "test-inputs.volatile-environment has unknown verifier capability {name:?}"
            ));
        }
    }
    volatile_environment.sort();
    if volatile_environment
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err("test-inputs.volatile-environment repeats a variable".to_owned());
    }

    let mut hash = aterm_digest::Sha256::new();
    part(&mut hash, b"domain", b"aterm-test-package-inputs-v1");
    part(&mut hash, b"target", target.as_bytes());
    part(&mut hash, b"content-only-declaration", b"true");
    part(&mut hash, b"read-only-declaration", b"true");
    for (label, declared, base) in roots {
        part(&mut hash, b"root-kind", label);
        part(&mut hash, b"root-declaration", declared.as_bytes());
        let root = declared_path(&base, &declared)?;
        hash_tree(&mut hash, &root, &root)?;
    }
    for name in &volatile_environment {
        part(
            &mut hash,
            b"volatile-environment-declaration",
            name.as_bytes(),
        );
    }
    for file in [
        workspace.join("rust-toolchain.toml"),
        workspace.join("trust.lock"),
        workspace.join(".cargo/config.toml"),
        workspace.join(".cargo/config"),
    ] {
        part(
            &mut hash,
            b"toolchain-file",
            &relative_bytes(&workspace, &file),
        );
        if file.exists() {
            hash_tree(&mut hash, &file, &file)?;
        } else {
            part(&mut hash, b"absent", b"");
        }
    }
    Ok(PackageInputs {
        digest: aterm_codec::hex::encode_lower(&hash.finalize()),
        volatile_environment,
    })
}

fn target_key(spec: &str) -> Result<String, String> {
    let words: Vec<&str> = spec.split_whitespace().collect();
    for (index, word) in words.iter().enumerate() {
        match *word {
            "--lib" => return Ok("lib".to_owned()),
            "--test" | "--bin" | "--bench" | "--example" => {
                let name = words
                    .get(index + 1)
                    .filter(|name| !name.starts_with('-'))
                    .ok_or_else(|| format!("test spec {spec:?} has no target name"))?;
                return Ok(format!("{}:{name}", word.trim_start_matches('-')));
            }
            _ => {}
        }
    }
    Err(format!("test spec {spec:?} does not name a Cargo target"))
}

fn target_key_from_metadata(target: &str) -> Result<(), String> {
    let valid = target == "lib"
        || ["test:", "bin:", "bench:", "example:"]
            .iter()
            .any(|prefix| {
                target
                    .strip_prefix(prefix)
                    .is_some_and(|name| !name.is_empty() && !name.chars().any(char::is_whitespace))
            });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "test-inputs target {target:?} is not lib, test:<name>, bin:<name>, bench:<name> or example:<name>"
        ))
    }
}

fn workspace_root(package: &Path) -> Result<PathBuf, String> {
    for candidate in package.ancestors() {
        let manifest = candidate.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(value) = aterm_toml::from_str::<aterm_toml::Value>(&text) else {
            continue;
        };
        if value.get("workspace").is_some() {
            return candidate
                .canonicalize()
                .map_err(|error| format!("workspace root {}: {error}", candidate.display()));
        }
    }
    Err(format!(
        "no Cargo workspace contains package {}",
        package.display()
    ))
}

fn declared_path(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("declared input {relative:?} escapes its base"));
    }
    let path = base.join(relative);
    let mut prefix = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        prefix.push(name);
        let metadata = std::fs::symlink_metadata(&prefix)
            .map_err(|error| format!("declared input {}: {error}", prefix.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("declared input {} is a symlink", prefix.display()));
        }
    }
    let physical = path
        .canonicalize()
        .map_err(|error| format!("declared input {}: {error}", path.display()))?;
    if !physical.starts_with(base) {
        return Err(format!(
            "declared input {} escapes {}",
            path.display(),
            base.display()
        ));
    }
    Ok(physical)
}

fn hash_tree(hash: &mut aterm_digest::Sha256, root: &Path, path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("runtime input {}: {error}", path.display()))?;
    let relative = relative_bytes(root, path);
    if metadata.file_type().is_symlink() {
        return Err(format!("runtime input {} is a symlink", path.display()));
    }
    if metadata.is_file() {
        part(hash, b"file", &relative);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            part(hash, b"mode", &metadata.permissions().mode().to_le_bytes());
        }
        let mut file = File::open(path)
            .map_err(|error| format!("runtime input {}: {error}", path.display()))?;
        let mut buffer = vec![0_u8; 1 << 20];
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => part(hash, b"bytes", &buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(format!("runtime input {}: {error}", path.display()));
                }
            }
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!(
            "runtime input {} is not a file or directory",
            path.display()
        ));
    }
    part(hash, b"directory", &relative);
    let mut entries = std::fs::read_dir(path)
        .map_err(|error| format!("runtime input {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("runtime input {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| os_bytes(&entry.file_name()));
    for entry in entries {
        hash_tree(hash, root, &entry.path())?;
    }
    Ok(())
}

fn relative_bytes(root: &Path, path: &Path) -> Vec<u8> {
    path.strip_prefix(root).map_or_else(
        |_| os_bytes(path.as_os_str()),
        |path| os_bytes(path.as_os_str()),
    )
}

fn digest_file(path: &Path) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        let physical = path.canonicalize().map_err(|error| error.to_string())?;
        return digest_file(&physical);
    }
    if !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hash.update(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

/// Whether two absolute paths resolve to the same filesystem object. Each path's spelling still
/// stays in the command/environment identity, so a symlink or hard-link alias has its own key even
/// though it is accepted as the same physical executable.
fn same_physical_path(left: &Path, right: &Path) -> Result<bool, String> {
    let resolve = |path: &Path| {
        path.canonicalize()
            .map_err(|error| format!("runtime path {}: {error}", path.display()))
    };
    let left = resolve(left)?;
    let right = resolve(right)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let left_metadata = std::fs::metadata(&left)
            .map_err(|error| format!("runtime path {}: {error}", left.display()))?;
        let right_metadata = std::fs::metadata(&right)
            .map_err(|error| format!("runtime path {}: {error}", right.display()))?;
        Ok(left_metadata.dev() == right_metadata.dev()
            && left_metadata.ino() == right_metadata.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(left == right)
    }
}

/// Conservative execution identity of a file this test executes. Bytes alone are insufficient:
/// an equal executable which moved behind a symlink or whose execution metadata changed must run
/// rather than inherit a prior pass. Unix ctime binds ACL/xattr changes without reading or
/// recording their possibly platform-specific values; atime is deliberately absent because this
/// verifier's own read can move it.
pub(crate) fn runtime_file_identity(path: &Path) -> Result<Vec<u8>, String> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut identity = os_bytes(path.as_os_str());
    #[cfg(unix)]
    let stat = {
        use std::os::unix::fs::MetadataExt as _;
        let mut stat = Vec::new();
        for value in [
            metadata.dev(),
            metadata.ino(),
            u64::from(metadata.mode()),
            metadata.nlink(),
            u64::from(metadata.uid()),
            u64::from(metadata.gid()),
            metadata.rdev(),
            metadata.size(),
            metadata.blocks(),
        ] {
            stat.extend(value.to_le_bytes());
        }
        for value in [
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ] {
            stat.extend(value.to_le_bytes());
        }
        stat
    };
    #[cfg(not(unix))]
    let stat = vec![u8::from(metadata.permissions().readonly())];
    identity.push(0);
    identity.extend(stat);
    Ok(identity)
}

fn command_identity(
    hash: &mut aterm_digest::Sha256,
    command: &Cmd,
    env: &BTreeMap<Vec<u8>, Vec<u8>>,
) {
    part(
        hash,
        b"cwd",
        &command
            .cwd
            .as_deref()
            .map_or_else(Vec::new, |path| os_bytes(path.as_os_str())),
    );
    part(hash, b"argv", &os_bytes(command.program.as_os_str()));
    for argument in &command.args {
        part(hash, b"argv", &os_bytes(argument));
    }
    for (name, value) in env {
        part(hash, b"env-name", name);
        part(hash, b"env-value", value);
    }
}

fn effective_env(command: &Cmd) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut env = BTreeMap::new();
    for (name, value) in &command.envs {
        let mut name = os_bytes(name);
        if cfg!(windows) {
            name.make_ascii_uppercase();
        }
        env.insert(name, os_bytes(value));
    }
    env
}

/// The exact finalized environment, except for verifier capabilities a bounded target declares
/// that it does not inspect. Their concrete values are deliberately per invocation: the run
/// record names this verifier process, and the private home and subprocess-slot directory live
/// below its random scratch root. Their policy and implementation are already bound by the
/// verifier digest. A target which reads one names it in `volatile-environment`, making its bytes
/// exact. The field accepts only the capabilities below, so a misspelling fails closed.
fn content_environment(
    exact: &BTreeMap<Vec<u8>, Vec<u8>>,
    scratch: &Path,
    declared: &[String],
) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let declared = |name: &[u8]| {
        declared.iter().any(|item| {
            let mut item = item.as_bytes().to_vec();
            if cfg!(windows) {
                item.make_ascii_uppercase();
            }
            item == name
        })
    };
    exact
        .iter()
        .map(|(name, value)| {
            let value = if declared(name) {
                value.clone()
            } else if name == crate::runs::VERIFY_RUN_ENV.as_bytes() {
                b"$VERIFIER_RUN_RECORD".to_vec()
            } else if name == crate::testrun::SUBPROCESS_SLOTS_ENV.as_bytes() {
                b"$VERIFIER_SUBPROCESS_SLOTS".to_vec()
            } else if name == aterm_capacity::COVER_ENV.as_bytes() {
                b"$VERIFIER_CAPACITY_COVER".to_vec()
            } else if matches!(name.as_slice(), b"HOME" | b"USERPROFILE" | b"TMPDIR") {
                normalized_scratch_path(value, scratch)
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

fn normalized_scratch_path(value: &[u8], scratch: &Path) -> Vec<u8> {
    let value = os_from_bytes(value);
    let path = Path::new(&value);
    path.strip_prefix(scratch).map_or_else(
        |_| os_bytes(path.as_os_str()),
        |relative| {
            let mut normalized = b"$VERIFIER_SCRATCH".to_vec();
            let suffix = os_bytes(relative.as_os_str());
            if !suffix.is_empty() {
                normalized.push(std::path::MAIN_SEPARATOR as u8);
                normalized.extend_from_slice(&suffix);
            }
            normalized
        },
    )
}

fn part(hash: &mut aterm_digest::Sha256, name: &[u8], value: &[u8]) {
    hash.update((name.len() as u64).to_le_bytes());
    hash.update(name);
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn os_from_bytes(value: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt as _;
    OsStr::from_bytes(value).to_os_string()
}

#[cfg(not(unix))]
fn os_from_bytes(value: &[u8]) -> OsString {
    String::from_utf8_lossy(value).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrun::{Binary, Record};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Bed {
        _dir: aterm_tempfile::TempDir,
        root: PathBuf,
        package: PathBuf,
        exe: PathBuf,
        helper: PathBuf,
    }

    impl Bed {
        fn new(metadata: &str) -> Self {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path().join(format!(
                "workspace-{}",
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let package = root.join("crates/p");
            std::fs::create_dir_all(package.join("src")).unwrap();
            std::fs::create_dir_all(root.join("target/debug/deps")).unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[workspace]\nmembers = [\"crates/p\"]\n",
            )
            .unwrap();
            std::fs::write(root.join("trust.lock"), "build = 1\n").unwrap();
            std::fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname = \"p\"\nversion = \"0.0.0\"\n{metadata}\n"),
            )
            .unwrap();
            std::fs::write(package.join("src/lib.rs"), "pub fn answer() -> u8 { 1 }\n").unwrap();
            let exe = root.join("target/debug/deps/p-0000000000000000");
            let helper = root.join("target/debug/helper");
            std::fs::write(&exe, b"test executable").unwrap();
            std::fs::write(&helper, b"helper executable").unwrap();
            #[cfg(unix)]
            for executable in [&exe, &helper] {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            Self {
                _dir: dir,
                root,
                package,
                exe,
                helper,
            }
        }

        fn binary(&self) -> Binary {
            self.binary_for("-p p --lib")
        }

        fn binary_for(&self, spec: &str) -> Binary {
            Binary {
                header: String::new(),
                spec: spec.into(),
                harness: true,
                record: Record {
                    cwd: self.package.clone(),
                    argv: vec![self.exe.clone().into_os_string(), "--skip".into()],
                    env: vec![
                        ("CARGO_PKG_NAME".into(), "p".into()),
                        (
                            "CARGO_MANIFEST_DIR".into(),
                            self.package.clone().into_os_string(),
                        ),
                        (
                            "CARGO_BIN_EXE_helper".into(),
                            self.helper.clone().into_os_string(),
                        ),
                        ("PATH".into(), OsString::new()),
                        ("DUP".into(), "old".into()),
                        ("DUP".into(), "effective".into()),
                    ],
                },
            }
        }

        fn command(&self, binary: &Binary) -> Cmd {
            self.finalize(Cmd::replayed(
                &binary.record.argv,
                &binary.record.cwd,
                binary.record.env.clone(),
            ))
        }

        fn finalize(&self, command: Cmd) -> Cmd {
            crate::exec::finalize_exact_command(
                command.counting_waits(),
                crate::exec::ExecEnv {
                    events: None,
                    cwd: &self.package,
                    path: OsStr::new(""),
                    scratch: &self.root,
                    child_ceiling: None,
                    remove_env: &[],
                    add_env: &[],
                    stage_groups: None,
                    host: &crate::exec::LIVE_HOST,
                    hang_samples: None,
                },
            )
            .unwrap()
        }

        fn evidence(&self, binary: &Binary) -> Evidence {
            EvidenceBuilder::new(
                Some("11".repeat(32)),
                Some("22".repeat(32)),
                self.root.join("scratch"),
            )
            .evidence(binary, &self.command(binary))
        }
    }

    const BOUNDED: &str = "[package.metadata.aterm.test-inputs]\n\
        bounded = true\ncontent-only = true\nread-only = true\npaths = [\"fixtures\"]\nvolatile-environment = []";

    #[test]
    fn an_exact_replay_without_a_finalization_receipt_is_unbounded() {
        let bed = Bed::new(BOUNDED);
        std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
        let binary = bed.binary();
        let replayed = Cmd::replayed(
            &binary.record.argv,
            &binary.record.cwd,
            binary.record.env.clone(),
        );
        let evidence = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        )
        .evidence(&binary, &replayed);
        assert!(!evidence.bounded());
        assert!(
            evidence
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("finalized complete environment"))
        );
        assert!(bed.evidence(&binary).bounded());
    }

    #[test]
    fn fixture_context_environment_and_helper_bytes_are_in_the_key() {
        let bed = Bed::new(BOUNDED);
        std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
        let fixture = bed.package.join("fixtures/value.txt");
        std::fs::write(&fixture, "one").unwrap();
        let binary = bed.binary();
        let first = bed.evidence(&binary);
        assert!(first.bounded(), "{first:?}");

        std::fs::write(
            bed.package.join("src/lib.rs"),
            "pub fn answer() -> u8 { 2 } // another target stayed byte-equal\n",
        )
        .unwrap();
        assert_eq!(
            first.content_key,
            bed.evidence(&binary).content_key,
            "compiled source is represented by executable bytes, not double-counted as runtime input"
        );

        std::fs::write(&fixture, "two").unwrap();
        let fixture_changed = bed.evidence(&binary);
        assert_ne!(first.content_key, fixture_changed.content_key);
        std::fs::write(&fixture, "one").unwrap();

        let context_changed = EvidenceBuilder::new(
            Some("33".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        )
        .evidence(&binary, &bed.command(&binary));
        assert_ne!(first.content_key, context_changed.content_key);
        let verifier_changed = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("44".repeat(32)),
            bed.root.join("scratch"),
        )
        .evidence(&binary, &bed.command(&binary));
        assert_ne!(first.content_key, verifier_changed.content_key);

        std::fs::write(bed.root.join("trust.lock"), "build = 2\n").unwrap();
        assert_ne!(first.content_key, bed.evidence(&binary).content_key);
        std::fs::write(bed.root.join("trust.lock"), "build = 1\n").unwrap();

        std::fs::write(&bed.exe, b"changed harness and test executable").unwrap();
        let harness_changed = bed.evidence(&binary);
        assert_ne!(first.exe_sha256, harness_changed.exe_sha256);
        assert_ne!(first.content_key, harness_changed.content_key);
        std::fs::write(&bed.exe, b"test executable").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let bytes_equal = bed.evidence(&binary);
            std::fs::set_permissions(&bed.exe, std::fs::Permissions::from_mode(0o644)).unwrap();
            let not_executable = bed.evidence(&binary);
            assert_eq!(bytes_equal.exe_sha256, not_executable.exe_sha256);
            assert_ne!(bytes_equal.content_key, not_executable.content_key);
            std::fs::set_permissions(&bed.exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut env_changed = binary.clone();
        env_changed.record.env.push(("DUP".into(), "new".into()));
        assert_ne!(first.content_key, bed.evidence(&env_changed).content_key);

        let mut scheduling = bed.command(&binary);
        scheduling = bed.finalize(scheduling.env("RUST_TEST_THREADS", "2").env(
            crate::testrun::SUBPROCESS_SLOTS_ENV,
            bed.root.join("scratch/slots.first"),
        ));
        let scheduled_first = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        )
        .evidence(&binary, &scheduling);
        scheduling = bed.finalize(scheduling.env("RUST_TEST_THREADS", "19").env(
            crate::testrun::SUBPROCESS_SLOTS_ENV,
            bed.root.join("scratch/slots.other"),
        ));
        assert_ne!(
            scheduled_first.content_key,
            EvidenceBuilder::new(
                Some("11".repeat(32)),
                Some("22".repeat(32)),
                bed.root.join("scratch"),
            )
            .evidence(&binary, &scheduling)
            .content_key,
            "the exact scheduling ceiling is a run-time input"
        );

        let first_scratch = bed.root.join("scratch.first");
        let second_scratch = bed.root.join("scratch.second");
        let capability_command = |scratch: &Path, run: &str| {
            bed.finalize(
                bed.command(&binary)
                    .env("RUST_TEST_THREADS", "2")
                    .env("HOME", scratch.join("release-test-home"))
                    .env(
                        crate::testrun::SUBPROCESS_SLOTS_ENV,
                        scratch.join("test-subprocs.random.noindex"),
                    )
                    .env(crate::runs::VERIFY_RUN_ENV, run)
                    .env(aterm_capacity::COVER_ENV, format!("cover-{run}")),
            )
        };
        let first_invocation = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            first_scratch.clone(),
        )
        .evidence(&binary, &capability_command(&first_scratch, "101-1001"));
        let second_invocation = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            second_scratch.clone(),
        )
        .evidence(&binary, &capability_command(&second_scratch, "202-2002"));
        assert_eq!(
            first_invocation.content_key, second_invocation.content_key,
            "two verifier invocations carry when only capabilities the target declares unread change"
        );

        std::fs::write(&bed.helper, b"changed helper executable").unwrap();
        assert_ne!(first.content_key, bed.evidence(&binary).content_key);
    }

    /// Every path which selects execution or an input root is absolute, and the command hashes the
    /// file it actually executes rather than trusting a parallel record field. A symlink alias to
    /// that physical executable remains eligible, but its invocation spelling gets a distinct key.
    /// NEGATIVE CONTROLS: a relative program/cwd/manifest/helper or a substituted program is
    /// unbounded; removing any corresponding production check makes that arm bounded.
    #[test]
    fn ambiguous_execution_and_input_paths_are_unbounded() {
        let bed = Bed::new(BOUNDED);
        std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
        std::fs::write(bed.package.join("fixtures/value.txt"), "value").unwrap();
        let binary = bed.binary();
        let builder = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        );
        let reason = |command: Cmd| {
            let evidence = builder.evidence(&binary, &command);
            assert!(!evidence.bounded(), "{evidence:?}");
            evidence.reason.unwrap_or_default()
        };

        let mut relative_program = bed.command(&binary);
        relative_program.program = "target/debug/deps/p-0000000000000000".into();
        assert!(
            reason(relative_program).contains("paths must be absolute"),
            "a relative executable must fail closed"
        );

        let mut substituted_program = bed.command(&binary);
        substituted_program.program.clone_from(&bed.helper);
        assert!(
            reason(substituted_program).contains("not its recorded executable"),
            "bytes from a different command program must not be attributed to the record"
        );

        let mut no_cwd = bed.command(&binary);
        no_cwd.cwd = None;
        assert!(reason(no_cwd).contains("no exact working directory"));
        let mut relative_cwd = bed.command(&binary);
        relative_cwd.cwd = Some("crates/p".into());
        assert!(reason(relative_cwd).contains("working directory is not absolute"));
        let mut missing_cwd = bed.command(&binary);
        missing_cwd.cwd = Some(bed.root.join("missing-cwd"));
        assert!(reason(missing_cwd).contains("cannot be resolved"));
        let mut file_cwd = bed.command(&binary);
        file_cwd.cwd = Some(bed.helper.clone());
        assert!(reason(file_cwd).contains("is not a directory"));
        let direct = builder.evidence(&binary, &bed.command(&binary));
        let mut other_cwd = bed.command(&binary);
        other_cwd.cwd = Some(bed.root.clone());
        let other_cwd = bed.finalize(other_cwd);
        let other_cwd = builder.evidence(&binary, &other_cwd);
        assert!(other_cwd.bounded(), "{other_cwd:?}");
        assert_ne!(
            direct.content_key, other_cwd.content_key,
            "Cargo's non-package cwd is valid, but remains an execution input"
        );

        let relative_manifest =
            bed.finalize(bed.command(&binary).env("CARGO_MANIFEST_DIR", "crates/p"));
        assert!(reason(relative_manifest).contains("CARGO_MANIFEST_DIR is not absolute"));
        let relative_helper = bed.finalize(
            bed.command(&binary)
                .env("CARGO_BIN_EXE_helper", "target/debug/helper"),
        );
        assert!(reason(relative_helper).contains("names a non-absolute helper"));

        #[cfg(unix)]
        {
            let alias = bed.root.join("target/debug/deps/p-alias");
            std::os::unix::fs::symlink(&bed.exe, &alias).unwrap();
            let mut through_alias = bed.command(&binary);
            through_alias.program = alias;
            let through_alias = bed.finalize(through_alias);
            let through_alias = builder.evidence(&binary, &through_alias);
            assert!(direct.bounded() && through_alias.bounded());
            assert_eq!(direct.exe_sha256, through_alias.exe_sha256);
            assert_ne!(
                direct.content_key, through_alias.content_key,
                "the actual symlink invocation path remains in the command identity"
            );

            let cwd_alias = bed.root.join("cwd-alias");
            std::os::unix::fs::symlink(&bed.package, &cwd_alias).unwrap();
            let mut through_cwd_alias = bed.command(&binary);
            through_cwd_alias.cwd = Some(cwd_alias.clone());
            let through_cwd_alias = bed.finalize(through_cwd_alias);
            let first_cwd_target = builder.evidence(&binary, &through_cwd_alias);
            assert!(first_cwd_target.bounded(), "{first_cwd_target:?}");
            std::fs::remove_file(&cwd_alias).unwrap();
            std::os::unix::fs::symlink(&bed.root, &cwd_alias).unwrap();
            let second_cwd_target = builder.evidence(&binary, &through_cwd_alias);
            assert!(second_cwd_target.bounded(), "{second_cwd_target:?}");
            assert_ne!(
                first_cwd_target.content_key, second_cwd_target.content_key,
                "retargeting an unchanged cwd symlink must change the content key"
            );
        }
    }

    #[test]
    fn undeclared_and_escaping_readers_are_unbounded() {
        let undeclared = Bed::new("");
        let evidence = undeclared.evidence(&undeclared.binary());
        assert!(!evidence.bounded());
        assert!(
            evidence
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("does not declare")
        );

        let undeclared_environment = Bed::new(
            "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\npaths = []",
        );
        let evidence = undeclared_environment.evidence(&undeclared_environment.binary());
        assert!(!evidence.bounded());
        assert!(
            evidence
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("volatile environment read set")
        );

        let metadata_observer = Bed::new(
            "[package.metadata.aterm.test-inputs]\n\
             bounded = true\nread-only = true\npaths = []\nvolatile-environment = []",
        );
        let evidence = metadata_observer.evidence(&metadata_observer.binary());
        assert!(!evidence.bounded());
        assert!(evidence.reason.unwrap().contains("content-only = true"));

        let external_tool = Bed::new(
            "[package.metadata.aterm.test-inputs]\n\
             bounded = true\ncontent-only = true\nread-only = true\npaths = []\n\
             volatile-environment = []\ntools = [\"git\"]",
        );
        let evidence = external_tool.evidence(&external_tool.binary());
        assert!(!evidence.bounded());
        assert!(evidence.reason.unwrap().contains("unknown field \"tools\""));

        let writable = Bed::new(
            "[package.metadata.aterm.test-inputs]\n\
             bounded = true\ncontent-only = true\npaths = []\nvolatile-environment = []",
        );
        let evidence = writable.evidence(&writable.binary());
        assert!(!evidence.bounded());
        assert!(evidence.reason.unwrap().contains("read-only = true"));

        let misspelled_capability = Bed::new(
            "[package.metadata.aterm.test-inputs]\n\
             bounded = true\ncontent-only = true\nread-only = true\npaths = []\n\
             volatile-environment = [\"ATERM_VERFIY_RUN\"]",
        );
        let evidence = misspelled_capability.evidence(&misspelled_capability.binary());
        assert!(!evidence.bounded());
        assert!(
            evidence
                .reason
                .unwrap()
                .contains("unknown verifier capability")
        );

        let escaping = Bed::new(
            "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\nvolatile-environment = []\nworkspace-paths = [\"../outside\"]",
        );
        let evidence = escaping.evidence(&escaping.binary());
        assert!(!evidence.bounded());
        assert!(evidence.reason.unwrap().contains("escapes"));

        #[cfg(unix)]
        {
            std::fs::create_dir_all(escaping.package.join("fixtures")).unwrap();
            std::fs::write(escaping.package.join("fixtures/value.txt"), "value").unwrap();
            std::os::unix::fs::symlink(
                escaping.package.join("fixtures"),
                escaping.package.join("linked"),
            )
            .unwrap();
            std::fs::write(
                escaping.package.join("Cargo.toml"),
                "[package]\nname = \"p\"\nversion = \"0.0.0\"\n\
                 [package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\nvolatile-environment = []\npaths = [\"linked/value.txt\"]\n",
            )
            .unwrap();
            let evidence = escaping.evidence(&escaping.binary());
            assert!(!evidence.bounded());
            assert!(evidence.reason.unwrap().contains("symlink"));
        }
    }

    #[test]
    fn a_declared_verifier_capability_is_hashed_exactly() {
        let bed = Bed::new(
            "[package.metadata.aterm.test-inputs]\n\
             bounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = [\"ATERM_VERIFY_RUN\"]",
        );
        let binary = bed.binary();
        let command =
            |run: &str| bed.finalize(bed.command(&binary).env(crate::runs::VERIFY_RUN_ENV, run));
        let builder = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        );
        assert_ne!(
            builder.evidence(&binary, &command("101-1001")).content_key,
            builder.evidence(&binary, &command("202-2002")).content_key,
            "a target which declares that it reads a capability gets its exact value"
        );
    }

    #[test]
    fn a_target_fixture_invalidates_only_its_declared_reader() {
        let bed = Bed::new(
            "[package.metadata.aterm.test-inputs]\n\
             bounded = true\n\
             content-only = true\n\
             read-only = true\n\
             paths = []\n\
             volatile-environment = []\n\
             [package.metadata.aterm.test-inputs.targets.\"test:reader\"]\n\
             paths = [\"fixtures\"]\n",
        );
        std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
        let fixture = bed.package.join("fixtures/value.txt");
        std::fs::write(&fixture, "before").unwrap();
        let reader = bed.binary_for("-p p --test reader");
        let sibling = bed.binary_for("-p p --test sibling");
        let reader_before = bed.evidence(&reader);
        let sibling_before = bed.evidence(&sibling);
        assert!(reader_before.bounded() && sibling_before.bounded());

        std::fs::write(&fixture, "after").unwrap();
        assert_ne!(reader_before.content_key, bed.evidence(&reader).content_key);
        assert_eq!(
            sibling_before.content_key,
            bed.evidence(&sibling).content_key,
            "a sibling target did not declare or read this fixture"
        );
    }

    #[test]
    fn proof_parser_refuses_fail_and_the_obsolete_sha_only_format() {
        let bed = Bed::new(
            "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = []",
        );
        let evidence = bed.evidence(&bed.binary());
        let mut proof = CarryProof::passed("-p p --lib", &evidence, 1.25);
        let mut passed = Passed::new();
        passed.insert(proof.spec.clone(), proof.clone());
        let encoded = encode_passed(&passed).unwrap();
        assert_eq!(read_passed(&encoded).unwrap(), passed);

        let legacy_without_json = encoded
            .lines()
            .map(|line| {
                let mut value: aterm_json::Value = aterm_json::from_str(line).unwrap();
                value.as_object_mut().unwrap().remove("json");
                aterm_json::to_string(&value).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            read_passed(&legacy_without_json).is_err(),
            "a proof which cannot reconstruct the prior invocation fails closed"
        );

        proof.outcome = "FAIL".into();
        let fail = format!("{}\n", aterm_json::to_string(&proof).unwrap());
        assert!(read_passed(&fail).unwrap_err().contains("only a prior ok"));
        assert!(read_passed(&format!("{} -p p --lib\n", "aa".repeat(32))).is_err());
    }
}
