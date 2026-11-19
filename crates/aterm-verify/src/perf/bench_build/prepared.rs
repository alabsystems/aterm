// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One compiler capture owns both executable artifacts for every runtime arm.

#[cfg(test)]
use std::fs::File;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Command;

use super::{Built, Entry, bench_profile, flags, load_average, set_bench_environment, units};
use crate::compiler_artifact::Artifact;
use crate::perf::arena::Halt;
use crate::perf::driver::built_executable;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildContext {
    pub(super) root: PathBuf,
    pub(super) root_identity: (u64, u64),
    pub(super) program: PathBuf,
    pub(super) build_dir: Option<String>,
}

/// Compilation is confined to preparation. Runtime arms retain this same context.
pub(crate) trait Compiler {
    fn context(&self) -> Result<BuildContext, Halt>;
    fn sources(&self) -> Result<Vec<flags::Source>, Halt>;
    fn audit(&self, args: &[&str]) -> Result<units::Audit, Halt>;
    fn compile(&self, args: &[&str]) -> Result<String, Halt>;
    fn runtime_command(&self, executable: &Path) -> Result<Command, Halt>;
}

/// Runtime entry and capture are separate from compiler admission.
pub(super) trait Runtime {
    type Admission;
    fn admit(&mut self) -> Result<Self::Admission, Halt>;
    fn validate(&mut self, admitted: &Self::Admission) -> Result<(), Halt>;
    fn capture(
        &mut self,
        command: &mut Command,
        progress: &str,
        name: &str,
    ) -> Result<String, Halt>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    profile: bench_profile::Profile,
    args: Vec<String>,
    bench: String,
}

impl Request {
    fn new(profile: bench_profile::Profile, args: &[&str]) -> Result<Self, Halt> {
        let mut normalized = Vec::new();
        for arg in args {
            if let Some((key, value)) = arg.split_once('=')
                && matches!(key, "--profile" | "--bench" | "--features" | "--package")
            {
                normalized.extend([
                    if key == "--package" { "-p" } else { key }.to_owned(),
                    value.to_owned(),
                ]);
            } else {
                normalized.push(if *arg == "--package" { "-p" } else { arg }.to_string());
            }
        }
        let one = |key: &str| -> Result<String, Halt> {
            let indices: Vec<usize> = normalized
                .iter()
                .enumerate()
                .filter_map(|(i, word)| (word == key).then_some(i))
                .collect();
            let [index] = indices.as_slice() else {
                return Err(Halt::Fail(format!(
                    "bench preparation requires exactly one {key}"
                )));
            };
            normalized
                .get(index + 1)
                .filter(|word| !word.is_empty() && !word.starts_with('-'))
                .cloned()
                .ok_or_else(|| Halt::Fail(format!("bench preparation has no value for {key}")))
        };
        let bench = one("--bench")?;
        if one("--profile")? != profile.cargo_profile()
            || normalized.iter().any(|word| {
                word == "--release" || word == "--no-run" || word.starts_with("--message-format")
            })
        {
            return Err(Halt::Fail(
                "bench request contradicts its profile or compiler capture".into(),
            ));
        }
        Ok(Self {
            profile,
            args: normalized,
            bench,
        })
    }

    fn words(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }
}

/// The original Cargo paths and held files remain alive through all runtime arms.
/// These bookends detect observable file changes; they do not prove mapped-image
/// identity or eliminate the narrow final validation-to-exec race.
pub(crate) struct PreparedBench<C: Compiler> {
    compiler: C,
    context: BuildContext,
    request: Request,
    built: Built,
    app: Artifact,
    bench: Artifact,
}

impl<C: Compiler> PreparedBench<C> {
    pub(super) fn prepare(
        compiler: C,
        profile: bench_profile::Profile,
        build_args: &[&str],
    ) -> Result<(Self, String), Halt> {
        let request = Request::new(profile, build_args)?;
        let context = compiler.context()?;
        let removed: Vec<String> = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| flags::is_env_source(key))
            .collect();
        let sources = compiler.sources()?;
        if let Some(why) = flags::refusal(&sources) {
            return Err(Halt::Fail(format!(
                "the `{}` arm's build is not what the arm says: {why}",
                profile.name()
            )));
        }
        let audit = compiler.audit(&request.words())?;
        if let Some(why) = audit.refusal(units::Promise::of(profile)) {
            return Err(Halt::Fail(format!(
                "the `{}` arm's build is not what the arm says: {why}",
                profile.name()
            )));
        }
        let mut lane_build = format!(
            "`{}` — {}. Cargo's plan, read before the run: {}; {}",
            profile.name(),
            profile.describe(),
            audit.summary(),
            flags::describe(&sources)
        );
        if !removed.is_empty() {
            lane_build.push_str(&format!(" (the caller's {} removed)", removed.join(", ")));
        }
        lane_build
            .push_str("; its identity record the constant one (no identity input reaches it)");
        eprintln!("  {lane_build}");
        let load_before = load_average();
        let messages = compiler.compile(&request.words())?;
        completed(&messages)?;
        let app_path = built_executable(&messages, "aterm", "bin")?;
        let bench_path = built_executable(&messages, &request.bench, "bench")?;
        if bench_profile::Profile::of_binary(&app_path) != Some(profile) {
            return Err(Halt::Fail(format!(
                "cargo built the `{}` arm's aterm at {}, outside the arm's `{}` directory",
                profile.name(),
                app_path.display(),
                profile.dir()
            )));
        }
        let app = Artifact::open(&app_path)?;
        let bench = Artifact::open(&bench_path)?;
        if app.canonical == bench.canonical || app.same_file(&bench) {
            return Err(Halt::Fail(
                "compiler app and bench artifacts alias one file".into(),
            ));
        }
        let built = Built {
            path: app.canonical.clone(),
            identity: bench_profile::identity(&app.canonical)?,
            lane_build,
        };
        let prepared = Self {
            compiler,
            context,
            request,
            built,
            app,
            bench,
        };
        prepared.validate()?;
        // Evidence from this successful capture, not a new Cargo discovery or a freshness claim.
        for (role, original) in compiler_records(
            &messages,
            &prepared.app.selected,
            &prepared.request.bench,
            &prepared.bench.selected,
        )? {
            eprintln!("  bench-compiler-artifact {role}: {original}");
        }
        Ok((prepared, load_before))
    }

    pub(crate) fn built(&self) -> &Built {
        &self.built
    }

    /// A set of runtime worlds must all name this preparation before any world runs.
    pub(crate) fn require_request(
        &self,
        profile: bench_profile::Profile,
        args: &[&str],
    ) -> Result<(), Halt> {
        if Request::new(profile, args)? != self.request {
            return Err(Halt::Fail(
                "runtime world names another prepared bench request".into(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), Halt> {
        if self.compiler.context()? != self.context {
            return Err(Halt::Fail(
                "bench compiler context changed after preparation".into(),
            ));
        }
        self.app.validate()?;
        self.bench.validate()?;
        Ok(())
    }

    pub(super) fn run<R: Runtime>(
        &self,
        knobs: (&str, &str),
        output: (&str, &Path),
        progress: &str,
        entry: Entry,
        runtime: &mut R,
    ) -> Result<String, Halt> {
        let admitted = match entry {
            Entry::Timed => Some(runtime.admit()?),
            Entry::Counted => None,
        };
        // Admission can wait while another build replaces either artifact.
        self.validate()?;
        let mut command = self.compiler.runtime_command(&self.bench.canonical)?;
        command.arg("--bench");
        set_bench_environment(&mut command, knobs.0, knobs.1, output, &self.built.path);
        eprintln!(
            "  $ ATERM_TEST_EXECUTABLE={:?} {}=\"{}\" {}={:?} {} --bench",
            self.built.path,
            knobs.0,
            knobs.1,
            output.0,
            output.1,
            self.bench.canonical.display()
        );
        let result = runtime.capture(&mut command, progress, &self.request.bench);
        // A successful capture has settled its child and output reader. Errors still
        // run these bookends, but cannot establish a completed take.
        let artifacts = self.validate();
        let timing = admitted
            .as_ref()
            .map(|guard| runtime.validate(guard))
            .transpose();
        artifacts?;
        timing?;
        result
    }
}

/// Original selected artifact records, within the existing 16 MiB complete-output bound.
/// Duplicate identical-path records remain in order; absent `fresh` stays absent.
fn compiler_records<'a>(
    messages: &'a str,
    app: &Path,
    bench_name: &str,
    bench: &Path,
) -> Result<Vec<(&'static str, &'a str)>, Halt> {
    const LIMIT: usize = 16 * 1024 * 1024;
    const PREFIX: &str = "  bench-compiler-artifact ";
    let mut bytes = 0_usize;
    let mut records = Vec::new();
    for original in messages.lines() {
        let Ok(crate::compiler_message::UnitMessage::Unit(message)) =
            aterm_json::from_str::<crate::compiler_message::UnitMessage>(original)
        else {
            continue;
        };
        if message.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(executable) = message
            .get("executable")
            .and_then(aterm_json::Value::as_str)
        else {
            continue;
        };
        let name = message
            .pointer("/target/name")
            .and_then(aterm_json::Value::as_str);
        if name != Some("aterm") && name != Some(bench_name) {
            continue;
        }
        let kinds =
            crate::compiler_message::artifact_target_kinds(&message).map_err(str::to_owned)?;
        for (role, target, kind, selected) in [
            ("app", "aterm", "bin", app),
            ("bench", bench_name, "bench", bench),
        ] {
            if name != Some(target)
                || !kinds.contains(&kind)
                || selected.as_os_str() != std::ffi::OsStr::new(executable)
            {
                continue;
            }
            bytes = bytes
                .checked_add(PREFIX.len() + role.len() + 3)
                .and_then(|count| count.checked_add(original.len()))
                .filter(|count| *count <= LIMIT)
                .ok_or_else(|| {
                    Halt::Fail(
                        "selected compiler-artifact receipt exceeds 16 MiB complete-output bound"
                            .into(),
                    )
                })?;
            records.push((role, original));
        }
    }
    if !["app", "bench"]
        .iter()
        .all(|role| records.iter().any(|(found, _)| found == role))
    {
        return Err(Halt::Fail(
            "selected compiler-artifact receipt is incomplete".into(),
        ));
    }
    Ok(records)
}

fn completed(messages: &str) -> Result<(), Halt> {
    let mut finishes = 0;
    for line in messages.lines() {
        let Ok(value) = aterm_json::from_str::<crate::compiler_message::CheckedValue>(line) else {
            continue;
        };
        let (value, duplicate) = value.into_parts();
        if duplicate {
            return Err(Halt::Fail(
                "bench capture has ambiguous message fields".into(),
            ));
        }
        if value.get("reason").and_then(aterm_json::Value::as_str) == Some("build-finished") {
            if value.get("success").and_then(aterm_json::Value::as_bool) != Some(true) {
                return Err(Halt::Fail(
                    "bench capture has no unambiguous successful completion".into(),
                ));
            }
            finishes += 1;
        }
    }
    if finishes != 1 {
        return Err(Halt::Fail(
            "bench capture requires one successful build-finished message".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "prepared_tests.rs"]
mod tests;
