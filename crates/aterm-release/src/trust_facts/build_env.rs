// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Connect a rustc dep-info file to the build-script runs in its own Targo fingerprint.

use super::build_files::{Packages, ScriptSource};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct Fingerprint {
    // Targo's DepFingerprint: package identity, extern crate name, public edge, content hash.
    deps: Vec<(u64, String, bool, u64)>,
}

#[derive(serde::Deserialize)]
struct ScriptInputs {
    local: Vec<crate::cutter_inputs::ScriptInput>,
}

struct Run {
    stdout: PathBuf,
    unit: String,
    record: Vec<u8>,
}

fn fingerprint(path: &Path) -> io::Result<Option<(Fingerprint, Vec<u8>)>> {
    let bytes = match crate::durable::read_file(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let decoded = aterm_json::from_slice(&bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid fingerprint at {}", path.display()),
        )
    })?;
    Ok(Some((decoded, bytes)))
}

fn plain_dir(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a real directory", path.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn names(path: &Path) -> io::Result<Vec<String>> {
    if !plain_dir(path)? {
        return Ok(Vec::new());
    }
    fs::read_dir(path)?
        .filter_map(|entry| match entry {
            Ok(entry) => entry.file_name().into_string().ok().map(Ok),
            Err(e) => Some(Err(e)),
        })
        .collect()
}

struct Units {
    profile: PathBuf,
    package: String,
    nested: bool,
}

impl Units {
    // The caller discovered dep by traversing real directories in this slot.
    fn for_dep(dep: &Path, hash: &str) -> io::Result<Option<Self>> {
        let Some(parent) = dep.parent() else {
            return Ok(None);
        };
        if parent.ends_with("deps") || parent.ends_with("examples") {
            let Some(profile) = parent.parent() else {
                return Ok(None);
            };
            if !plain_dir(profile)? || !plain_dir(parent)? {
                return Ok(None);
            }
            let suffix = format!("-{hash}");
            let mut candidates = names(&profile.join(".fingerprint"))?
                .into_iter()
                .filter_map(|unit| unit.strip_suffix(&suffix).map(str::to_owned));
            let Some(package) = candidates.next() else {
                return Ok(None);
            };
            if candidates.next().is_some() || package.is_empty() {
                return Ok(None);
            }
            return Ok(Some(Self {
                profile: profile.to_owned(),
                package,
                nested: false,
            }));
        }
        if let Some(build) = parent.parent().filter(|p| p.ends_with("build"))
            && let Some((package, id)) = parent
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.rsplit_once('-'))
        {
            if id != hash || package.is_empty() || !plain_dir(build)? || !plain_dir(parent)? {
                return Ok(None);
            }
            return Ok(build.parent().map(|profile| Self {
                profile: profile.to_owned(),
                package: package.to_owned(),
                nested: false,
            }));
        }
        let Some(unit) = parent
            .parent()
            .filter(|unit| parent.ends_with("out") && unit.ends_with(hash))
        else {
            return Ok(None);
        };
        let Some(package) = unit.parent() else {
            return Ok(None);
        };
        let Some(build) = package.parent().filter(|p| p.ends_with("build")) else {
            return Ok(None);
        };
        if !plain_dir(build)? || !plain_dir(package)? || !plain_dir(unit)? || !plain_dir(parent)? {
            return Ok(None);
        }
        let (Some(profile), Some(package)) =
            (build.parent(), package.file_name().and_then(|s| s.to_str()))
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            profile: profile.to_owned(),
            package: package.to_owned(),
            nested: true,
        }))
    }

    fn catalog(&self) -> PathBuf {
        if self.nested {
            self.profile.join("build").join(&self.package)
        } else {
            self.profile.join(".fingerprint")
        }
    }

    fn unit_name(&self, hash: &str) -> String {
        if self.nested {
            hash.to_owned()
        } else {
            format!("{}-{hash}", self.package)
        }
    }

    fn fingerprints(&self, hash: &str) -> io::Result<Option<PathBuf>> {
        let unit = self.catalog().join(self.unit_name(hash));
        if !plain_dir(&unit)? {
            return Ok(None);
        }
        Ok(Some(if self.nested {
            unit.join("fingerprint")
        } else {
            unit
        }))
    }

    fn stdout(&self, hash: &str) -> io::Result<Option<PathBuf>> {
        let build = self.profile.join("build");
        if !plain_dir(&build)? {
            return Ok(None);
        }
        let unit = if self.nested {
            self.catalog().join(hash).join("run")
        } else {
            build.join(self.unit_name(hash))
        };
        Ok(plain_dir(&unit)?.then(|| unit.join(if self.nested { "stdout" } else { "output" })))
    }

    fn producer(
        &self,
        package_id: u64,
        script: &str,
        edge: u64,
        raw_target: Option<&str>,
    ) -> io::Result<Option<Run>> {
        let marker = super::hex(&edge.to_le_bytes());
        let mut matched = None;
        for unit in names(&self.catalog())? {
            let id = if self.nested {
                unit.as_str()
            } else if let Some((_, id)) = unit
                .rsplit_once('-')
                .filter(|(pkg, _)| *pkg == self.package)
            {
                id
            } else {
                continue;
            };
            if !aterm_codec::hex::is_lower(id, 16) {
                continue;
            }
            let Some(dir) = self.fingerprints(id)? else {
                continue;
            };
            for file in names(&dir)? {
                let Some(target) = file.strip_prefix("run-build-script-") else {
                    continue;
                };
                if raw_target.is_some_and(|raw| raw != target) || target.replace('-', "_") != script
                {
                    continue;
                }
                // The unit directory identifies a build configuration. The marker inside
                // it is the content hash that the compiling unit actually recorded.
                if crate::durable::read_regular_bytes(&dir.join(&file), 16)? != marker.as_bytes() {
                    continue;
                }
                let Some((run, record)) = fingerprint(&dir.join(format!("{file}.json")))? else {
                    return Ok(None);
                };
                if !run
                    .deps
                    .iter()
                    .any(|(pkg, name, _, _)| *pkg == package_id && name == script)
                {
                    return Ok(None);
                }
                if matched.is_some() {
                    return Ok(None);
                }
                let Some(stdout) = self.stdout(id)? else {
                    return Ok(None);
                };
                matched = Some(Run {
                    stdout,
                    unit: id.to_owned(),
                    record,
                });
            }
        }
        Ok(matched)
    }

    fn runs(
        &self,
        consumer: &Fingerprint,
        declared: Option<&BTreeMap<String, ScriptSource>>,
        files: &dyn Fn(&str, &Path) -> bool,
    ) -> io::Result<Option<Vec<Run>>> {
        let mut runs = Vec::new();
        if let Some(declared) = declared {
            // Targo attaches one run per custom-build target of the consumer's own
            // package. An ordinary dependency may have the very same extern name.
            for (target, source) in declared {
                if !source.covered_by(|path| files(&self.package, path)) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "no keyed build-script source for {target} in package {}",
                            self.package
                        ),
                    ));
                }
                let crate_name = target.replace('-', "_");
                let mut matched = None;
                for (pkg, name, _, edge) in &consumer.deps {
                    if name == &crate_name
                        && let Some(run) = self.producer(*pkg, name, *edge, Some(target))?
                    {
                        if matched.is_some() {
                            return Ok(None);
                        }
                        matched = Some(run);
                    }
                }
                let Some(run) = matched else {
                    return Ok(None);
                };
                runs.push(run);
            }
        } else {
            // Without keyed target metadata every possible script edge needs its
            // original run evidence, including names also used by normal libraries.
            for (pkg, name, _, edge) in &consumer.deps {
                if name.starts_with("build_script_") || name.starts_with("metabuild_") {
                    let Some(run) = self.producer(*pkg, name, *edge, None)? else {
                        return Ok(None);
                    };
                    runs.push(run);
                }
            }
        }
        Ok(Some(runs))
    }
}

fn unescape(s: &str) -> Option<Cow<'_, str>> {
    if !s.contains('\\') {
        return Some(Cow::Borrowed(s));
    }
    let mut decoded = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        decoded.push(if c == '\\' {
            match chars.next()? {
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            }
        } else {
            c
        });
    }
    Some(Cow::Owned(decoded))
}

fn directives(stdout: &[u8]) -> impl DoubleEndedIterator<Item = (&str, &str)> {
    stdout.split(|b| *b == b'\n').filter_map(|line| {
        let line = std::str::from_utf8(line).ok()?.trim();
        let line = line
            .strip_prefix("targo:")
            .or_else(|| line.strip_prefix("cargo:"))?;
        line.strip_prefix(':').unwrap_or(line).split_once('=')
    })
}

fn emitted<'a>(stdout: &'a [u8], name: &str) -> Option<&'a str> {
    directives(stdout).rev().find_map(|(kind, arg)| {
        if kind != "rustc-env" {
            return None;
        }
        let (key, value) = arg.split_once('=')?;
        (key == name).then_some(value)
    })
}

// Targo observes rerun-if-env-changed in its own environment. An absent value
// there cannot establish absence in the environment Targo gives the script.
fn script_env(name: &str) -> bool {
    name.starts_with("CARGO_")
        || name.starts_with("TARGO_")
        || name.starts_with("DEP_")
        || matches!(
            name,
            "CARGO"
                | "OUT_DIR"
                | "HOST"
                | "TARGET"
                | "PROFILE"
                | "DEBUG"
                | "OPT_LEVEL"
                | "NUM_JOBS"
                | "RUSTC"
                | "RUSTDOC"
                | "RUSTC_LINKER"
                | "RUSTC_WRAPPER"
                | "RUSTC_WORKSPACE_WRAPPER"
                | "DYLD_FALLBACK_LIBRARY_PATH"
                | "LD_LIBRARY_PATH"
        )
}

fn covered_inputs(
    stdout: &[u8],
    run: &Run,
    units: &Units,
    lane: &super::LaneEnv,
    files: &dyn Fn(&str, &Path) -> bool,
) -> io::Result<bool> {
    // Targo can record only an environment dependency for a script. Its package
    // must still be uniquely identified inside the measured tree.
    if !files(&units.package, Path::new("Cargo.toml")) {
        return Ok(false);
    }
    let mut declared = BTreeSet::new();
    let mut explicit_files = false;
    let mut files_covered = true;
    for (kind, arg) in directives(stdout) {
        if kind == "rerun-if-env-changed" {
            if !declared.insert(arg) {
                return Ok(false);
            }
        } else if kind == "rerun-if-changed" {
            files_covered = files_covered && files(&units.package, Path::new(arg));
            explicit_files = true;
        }
    }
    // Read the supplying run's rows with the release cutter's strict Targo wire schema.
    // Its consumer's local rows describe a different unit and need no interpretation here.
    let Ok(inputs) = aterm_json::from_slice::<ScriptInputs>(&run.record) else {
        return Ok(false);
    };
    let mut observed = BTreeSet::new();
    let mut saw_files = false;
    for input in &inputs.local {
        use crate::cutter_inputs::ScriptInput;
        let (output, paths) = match input {
            ScriptInput::RerunIfEnvChanged { var, val } => {
                let var = var.as_str();
                if var.is_empty() || var.contains('\0') || !observed.insert(var) {
                    return Ok(false);
                }
                if lane.passed.contains(var) {
                    if !matches!(val, aterm_json::Value::Null | aterm_json::Value::String(_)) {
                        return Ok(false);
                    }
                    continue;
                }
                match (lane.pins.get(var), val) {
                    (Some(pin), aterm_json::Value::String(value)) if pin == value => {}
                    (None, aterm_json::Value::Null) if !script_env(var) => {}
                    _ => return Ok(false),
                }
                continue;
            }
            ScriptInput::RerunIfChanged { output, paths } => (output, paths),
            ScriptInput::RerunIfChangedContent {
                output,
                paths,
                snapshot: Some(snapshot),
            } if snapshot.valid() => (output, paths),
            ScriptInput::RerunIfChangedContent { snapshot: None, .. } => {
                return Err(io::Error::other(format!(
                    "Targo recorded no build-script content snapshot for package {}",
                    units.package
                )));
            }
            _ => return Ok(false),
        };
        // Targo writes the file observation relative to the slot build root, with
        // the profile (and, for a cross target, the triple) ahead of the unit.
        let Some(base) = units.profile.parent() else {
            return Ok(false);
        };
        let Ok(relative) = run.stdout.strip_prefix(base) else {
            return Ok(false);
        };
        if saw_files
            || !run.stdout.ends_with(output)
            || !output.ends_with(relative)
            || output.is_absolute()
            || paths.is_empty()
        {
            return Ok(false);
        }
        files_covered = files_covered && paths.iter().all(|p| files(&units.package, p));
        saw_files = true;
    }
    // Without an explicit rerun directive Targo fingerprints the package's default file list.
    if declared != observed || saw_files != (explicit_files || declared.is_empty()) {
        return Ok(false);
    }
    // Name a policy refusal only after matching the run's declarations and observations.
    // Never copy a path or an observed value from the build-script evidence into the report.
    if let Some(var) = declared.into_iter().find(|var| lane.passed.contains(*var)) {
        let label =
            if var.len() <= 128 && var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                var
            } else {
                "(name omitted)"
            };
        return Err(io::Error::other(format!(
            "build script for package {} declares host-passed environment variable {label}",
            units.package
        )));
    }
    if !files_covered {
        return Err(io::Error::other(format!(
            "build script for package {} has a file input the proof key does not cover",
            units.package
        )));
    }
    Ok(true)
}

// Targo records stdout first, then root-output. Only the former supplies rustc-env.
#[derive(serde::Deserialize)]
struct OutputReceipt {
    schema: u32,
    unit: String,
    outputs: [Output; 2],
}

#[derive(serde::Deserialize)]
struct Output {
    path_sha256: String,
    size: u64,
    sha256: String,
}

fn verified_stdout(path: &Path, unit: &str) -> io::Result<Option<Vec<u8>>> {
    let read = |path: &Path, limit| match crate::durable::read_regular_bytes(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    let Some(bytes) = read(&path.with_file_name("output-integrity-v2.json"), 16 << 10)? else {
        return Ok(None);
    };
    let receipt: OutputReceipt = aterm_json::from_slice(&bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid build-script output receipt",
        )
    })?;
    let [stdout, root] = receipt.outputs;
    if receipt.schema != 2 || receipt.unit != unit || stdout.size > 16 << 20 {
        return Ok(None);
    }
    let Some(bytes) = read(path, 16 << 20)? else {
        return Ok(None);
    };
    // Targo records an absolute path. A caller may reach it through an alias of the slot root.
    let digests = [std::path::absolute(path)?, fs::canonicalize(path)?]
        .map(|p| crate::publish::sha256_bytes(p.as_os_str().as_encoded_bytes()));
    Ok((digests.contains(&stdout.path_sha256)
        && !digests.contains(&root.path_sha256)
        && bytes.len() as u64 == stdout.size
        && crate::publish::sha256_bytes(&bytes) == stdout.sha256)
        .then_some(bytes))
}

fn matches_target(file: &str, crate_name: &str) -> bool {
    let Some(stem) = file.strip_suffix(".json") else {
        return false;
    };
    let stem = stem.strip_prefix("test-").unwrap_or(stem);
    [
        "lib-",
        "bin-",
        "example-",
        "bench-",
        "integration-test-",
        "build-script-",
    ]
    .iter()
    .any(|kind| {
        stem.strip_prefix(kind)
            .is_some_and(|target| target.replace('-', "_") == crate_name)
    })
}

/// The compiler normally writes a hashed dep-info name. A build-script compile can instead
/// write just its crate name in the selected unit's output directory.
fn dep_identity(dep: &Path) -> Option<(&str, &str)> {
    let stem = dep.file_stem()?.to_str()?;
    if let Some((name, hash)) = stem.rsplit_once('-') {
        return aterm_codec::hex::is_lower(hash, 16).then_some((name, hash));
    }
    let parent = dep.parent()?;
    let unit = parent.parent()?;
    let hash = if parent.ends_with("out") {
        unit.file_name()?.to_str()?
    } else if unit.ends_with("build") {
        parent.file_name()?.to_str()?.rsplit_once('-')?.1
    } else {
        return None;
    };
    aterm_codec::hex::is_lower(hash, 16).then_some((stem, hash))
}

/// Targo mirrors package metadata and its initial rustc target names into the native
/// namespace. The later primary-package and test-temporary settings have no native twin.
fn compiler_value(
    name: &str,
    value: Option<&str>,
    target: &str,
    crate_name: &str,
    package: &str,
    has_script: bool,
    binary_example: Option<bool>,
) -> bool {
    match name {
        "CARGO" => return value.is_some(),
        "OUT_DIR" => return has_script == value.is_some(),
        "CARGO_PRIMARY_PACKAGE" => return value.is_none_or(|v| v == "1"),
        "CARGO_TARGET_TMPDIR" => {
            return value.is_some()
                == (target.starts_with("integration-test-") || target.starts_with("bench-"));
        }
        _ => {}
    }
    match name
        .strip_prefix("CARGO_")
        .or_else(|| name.strip_prefix("TARGO_"))
    {
        Some("CRATE_NAME") => value == Some(crate_name),
        Some("BIN_NAME") => {
            if target.starts_with("bin-") {
                // Targo may give an ordinary binary a separate output filename.
                value.is_some()
            } else if let Some(example) = target.strip_prefix("example-") {
                binary_example.is_some_and(|binary| value == binary.then_some(example))
            } else {
                value.is_none()
            }
        }
        Some("PKG_NAME") => value == Some(package),
        Some(
            "MANIFEST_DIR" | "MANIFEST_PATH" | "PKG_VERSION" | "PKG_VERSION_MAJOR"
            | "PKG_VERSION_MINOR" | "PKG_VERSION_PATCH" | "PKG_VERSION_PRE" | "PKG_DESCRIPTION"
            | "PKG_HOMEPAGE" | "PKG_REPOSITORY" | "PKG_LICENSE" | "PKG_LICENSE_FILE"
            | "PKG_AUTHORS" | "PKG_RUST_VERSION" | "PKG_README",
        ) => value.is_some(),
        _ => value.is_none(),
    }
}

struct Consumer<'a> {
    target: String,
    crate_name: &'a str,
    package: String,
    script_outputs: Vec<Vec<u8>>,
    binary_example: Option<bool>,
}

fn lookup<'a>(
    dep: &'a Path,
    lane: &super::LaneEnv,
    packages: &Packages,
    files: &dyn Fn(&str, &Path) -> bool,
) -> io::Result<Option<Consumer<'a>>> {
    let Some((crate_name, hash)) = dep_identity(dep) else {
        return Ok(None);
    };
    let Some(units) = Units::for_dep(dep, hash)? else {
        return Ok(None);
    };
    let Some(dir) = units.fingerprints(hash)? else {
        return Ok(None);
    };
    let mut targets = names(&dir)?
        .into_iter()
        .filter(|file| matches_target(file, crate_name));
    let Some(target) = targets.next() else {
        return Ok(None);
    };
    if targets.next().is_some() {
        return Ok(None);
    }
    let Some((consumer, _)) = fingerprint(&dir.join(&target))? else {
        return Ok(None);
    };
    let target = target.strip_suffix(".json").unwrap_or(&target);
    let target = target.strip_prefix("test-").unwrap_or(target);
    let keyed = files(&units.package, Path::new("Cargo.toml"));
    let mut script_outputs = Vec::new();
    // Targo compiles the build script before running it. Its compiler can see
    // ordinary build-dependencies, including libraries with script-like names.
    let runs = if target.starts_with("build-script-") {
        Vec::new()
    } else {
        let declared = if keyed {
            let Some(scripts) = packages.build_scripts(&units.package) else {
                return Ok(None);
            };
            Some(scripts)
        } else {
            None
        };
        let Some(runs) = units.runs(&consumer, declared, files)? else {
            return Ok(None);
        };
        runs
    };
    for run in runs {
        let Some(stdout) = verified_stdout(&run.stdout, &run.unit)? else {
            return Ok(None);
        };
        // Even a script that did not write the name can conditionally set it on a host
        // where one of its declared inputs changes.
        if !covered_inputs(&stdout, &run, &units, lane, files)? {
            return Ok(None);
        }
        script_outputs.push(stdout);
    }
    let binary_example = target
        .strip_prefix("example-")
        .filter(|_| keyed)
        .and_then(|name| packages.example_is_binary(&units.package, name));
    Ok(Some(Consumer {
        target: target.to_owned(),
        binary_example,
        crate_name,
        package: units.package,
        script_outputs,
    }))
}

impl Consumer<'_> {
    fn records_read(&self, name: &str, value: Option<&str>, lane: &super::LaneEnv) -> bool {
        let mut supplied = false;
        for stdout in &self.script_outputs {
            if let Some(set) = emitted(stdout, name) {
                // When multiple attached scripts write this name their execution order
                // is unknown. All must agree.
                if Some(set) != value {
                    return false;
                }
                supplied = true;
            }
        }
        supplied
            || match lane.pins.get(name) {
                Some(pin) => value == Some(pin.as_str()),
                None => compiler_value(
                    name,
                    value,
                    &self.target,
                    self.crate_name,
                    &self.package,
                    !self.script_outputs.is_empty(),
                    self.binary_example,
                ),
            }
    }
}

fn decoded_read<'a>(
    name: &'a str,
    value: Option<&'a str>,
) -> Option<(Cow<'a, str>, Option<Cow<'a, str>>)> {
    let name = unescape(name)?;
    // Targo ignores this directive even when it appears in verified stdout.
    if name == "RUSTC_BOOTSTRAP" && value.is_some() {
        return None;
    }
    let value = match value {
        Some(raw) => Some(unescape(raw)?),
        None => None,
    };
    Some((name, value))
}

/// Read the consumer and authenticate each attached script once for this dep-info file.
/// Refusals retain only the original variable name and whether the compiler observed a value.
pub(super) fn refused_reads<'a>(
    dep: &Path,
    reads: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    lane: &super::LaneEnv,
    packages: &Packages,
    files: &dyn Fn(&str, &Path) -> bool,
) -> Result<Vec<(&'a str, bool)>, String> {
    let mut consumer = None;
    let mut refused = Vec::new();
    for (name, value) in reads {
        if !lane.passed.contains(name)
            && let Some((key, observed)) = decoded_read(name, value)
        {
            let evidence = consumer
                .get_or_insert_with(|| lookup(dep, lane, packages, files))
                .as_ref()
                .map_err(|e| {
                    format!(
                        "could not verify build-script inputs for {}: {e}",
                        dep.display()
                    )
                })?;
            if evidence
                .as_ref()
                .is_some_and(|c| c.records_read(&key, observed.as_deref(), lane))
            {
                continue;
            }
        }
        refused.push((name, value.is_some()));
    }
    Ok(refused)
}

#[cfg(test)]
#[path = "build_env_tests.rs"]
mod tests;
