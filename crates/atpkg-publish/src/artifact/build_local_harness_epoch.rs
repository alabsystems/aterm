// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Typed execution data for the existing artifact builder. Original retained
//! CPU/cohort custody remains with the caller's Owner; this module creates none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_capacity::epoch_recipe::{RootAnalysis, RootOutput, RootProfile, RootRecipe};

use super::{HarnessBuild, Profile, Ran, Tools, write_new};
use crate::artifact::build_local::{Built, CHECKSUM_FLAG, CONTENT_CONFIG, CacheFreshness, Policy};
use crate::artifact::pack;
use crate::ctx::Ctx;
use crate::error::{Error, Result};

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

fn refused() -> Error {
    Error::refused("the harness plan is outside the typed retained-owner build recipe")
}

impl HarnessBuild {
    /// Translate the actual checked plan without passing arbitrary arguments,
    /// executables or environment overrides into the resource owner.
    /// # Errors
    /// Unsupported lane, flags, environment or native compiler scope. Unsupported
    /// inputs refuse instead of silently changing the requested build.
    pub fn recipe(&self, root: u32) -> Result<RootRecipe> {
        let req = &self.req;
        let plan = &self.plan;
        let command = &plan.command;
        if root >= 64
            || req.policy != Policy::Unverified
            || command.program != plan.sysroot.join("bin/targo")
            || command.cwd.as_deref() != Some(self.checkout.as_path())
            || command.clear_env
            || command.deadline.is_some()
            || plan.bins.len() != 1
            || plan.metadata.expose != plan.bins
        {
            return Err(refused());
        }
        let arguments: Vec<_> = command
            .args
            .iter()
            .map(|arg| arg.to_str().ok_or_else(refused))
            .collect::<Result<_>>()?;
        if !arguments.starts_with(&["--unverified", "build"]) {
            return Err(refused());
        }
        let mut flags = BTreeSet::new();
        let mut profile = None;
        let mut jobs = None;
        let mut bin = None;
        let mut package = None;
        let mut features = Vec::new();
        let mut default_features = true;
        let mut all_features = false;
        let mut offline = false;
        let mut content = false;
        let mut checksum = false;
        let mut words = arguments[2..].iter().copied();
        while let Some(flag) = words.next() {
            let key = match flag {
                "-p" => "--package",
                "-F" => "--features",
                other => other,
            };
            if key != "--features" && !flags.insert(key) {
                return Err(refused());
            }
            match key {
                "--locked" => {}
                "--profile" => {
                    profile = Some(match words.next().ok_or_else(refused)? {
                        "dev" => RootProfile::Dev,
                        "release" => RootProfile::Release,
                        _ => return Err(refused()),
                    });
                }
                "-j" | "--jobs" => {
                    if jobs.is_some() {
                        return Err(refused());
                    }
                    jobs = Some(
                        words
                            .next()
                            .ok_or_else(refused)?
                            .parse::<u32>()
                            .map_err(|_| refused())?,
                    );
                }
                "--bin" => bin = Some(words.next().ok_or_else(refused)?.to_owned()),
                "--package" => package = Some(words.next().ok_or_else(refused)?.to_owned()),
                "--features" => features.push(words.next().ok_or_else(refused)?.to_owned()),
                "--no-default-features" => default_features = false,
                "--all-features" => all_features = true,
                "--offline" => offline = true,
                "--config" => {
                    if words.next() != Some(CONTENT_CONFIG) {
                        return Err(refused());
                    }
                    content = true;
                }
                CHECKSUM_FLAG => checksum = true,
                _ => return Err(refused()),
            }
        }
        let profile = profile.ok_or_else(refused)?;
        let jobs = jobs.ok_or_else(refused)?;
        let bin = bin.ok_or_else(refused)?;
        if content != checksum
            || content != (plan.cache_freshness == CacheFreshness::ContentInputsV1)
            || !flags.contains("--locked")
            || jobs != req.jobs
            || !(1..=256).contains(&jobs)
            || bin != plan.bins[0]
            || profile
                != match req.profile {
                    Profile::Dev => RootProfile::Dev,
                    Profile::Release => RootProfile::Release,
                }
        {
            return Err(refused());
        }
        let scrubbed: BTreeSet<_> = command.env_remove.iter().map(String::as_str).collect();
        if scrubbed != pack::SCRUBBED_ENV.iter().copied().collect() {
            return Err(refused());
        }
        let mut env = BTreeMap::new();
        for (key, value) in &command.env {
            if !matches!(
                key.as_str(),
                "PATH" | "RUSTC" | "RUSTDOC" | "CARGO_TARGET_DIR" | pack::HARNESS_VERSION_ENV
            ) {
                return Err(refused());
            }
            // Run and Command both apply the last value, including the checked
            // guarded harness target that supersedes the generic target.
            env.insert(key.as_str(), value);
        }
        let value = |key| env.get(key).copied().ok_or_else(refused);
        let rustc = PathBuf::from(value("RUSTC")?);
        let rustdoc = PathBuf::from(value("RUSTDOC")?);
        let path: Vec<_> = std::env::split_paths(value("PATH")?).collect();
        let identity = plan.harness.as_ref().ok_or_else(refused)?;
        if rustc != plan.sysroot.join("bin/trustc")
            || rustdoc != plan.sysroot.join("bin/trustdoc")
            || Path::new(value("CARGO_TARGET_DIR")?) != self.target.as_path()
            || value(pack::HARNESS_VERSION_ENV)?.to_str() != Some(identity.version.as_str())
            || path.first() != Some(&plan.sysroot.join("bin"))
            || path.iter().any(|part| !part.is_absolute())
        {
            return Err(refused());
        }
        Ok(RootRecipe {
            id: root,
            profile,
            bin,
            jobs,
            package,
            features,
            default_features,
            all_features,
            offline,
            content_inputs_v1: content,
            analysis: self
                .analysis_home
                .clone()
                .filter(|_| plan.analysis_session)
                .map(|cargo_home| RootAnalysis { cargo_home }),
            version: identity.version.clone(),
            rustc,
            rustdoc,
            path,
        })
    }

    /// Complete the shared artifact builder from one original closed-cohort
    /// output receipt. The caller drives Owner, including cancellation, before
    /// handing over this one-use result. This never creates a CPU supply.
    /// # Errors
    /// Foreign execution recipe/manifest/target, cancelled or failed root,
    /// incomplete/oversized output, changed source, or ordinary artifact checks.
    pub fn finish_root_output(
        self,
        ctx: &Ctx,
        tools: &dyn Tools,
        output: RootOutput,
    ) -> Result<Built> {
        self.finish_root_output_with_cache_work(ctx, tools, output)
            .and_then(|completion| completion.built)
    }

    /// Complete original artifact acceptance and return ancillary typed scan observations.
    /// Unavailable observations never rewrite the original native outcome or acceptance.
    /// # Errors
    /// The same original output, source, custody and artifact checks as `finish_root_output`.
    pub fn finish_root_output_with_cache_work(
        self,
        ctx: &Ctx,
        tools: &dyn Tools,
        output: RootOutput,
    ) -> Result<super::HarnessCompletion> {
        if self.home != ctx.home
            || output.recipe() != &self.recipe(output.outcome().id)?
            || output.manifest() != self.manifest()
            || output.target() != self.target()
        {
            return Err(Error::refused(
                "original owner output differs from the prepared harness execution",
            ));
        }
        let epoch = output.epoch().to_owned();
        let bytes = output
            .read(OUTPUT_LIMIT)
            .map_err(|e| Error::io("read complete original compiler output", &e))?;
        let cache_work = self
            .cache_work_collector(bytes.outcome.id)?
            .collect(tools, &bytes, &self.logs);
        let evidence = aterm_json::json!({
            "schema":1,"scope":"original-retained-owner-harness",
            "epoch":epoch,"root":bytes.outcome.id,"pid":bytes.outcome.pid,
            "code":bytes.outcome.code,"success":bytes.outcome.success,
            "cancelled":bytes.cancellation.is_some(),
            "manifest":self.manifest(),"target":self.target(),
        });
        write_new(
            &self.logs.join("execution-owner.json"),
            &aterm_json::to_vec(&evidence).map_err(|e| Error::cannot_run(e.to_string()))?,
        )?;
        if bytes.cancellation.is_some() || !bytes.outcome.success {
            write_new(&self.logs.join("build.stdout"), &bytes.stdout)?;
            write_new(&self.logs.join("build.stderr"), &bytes.stderr)?;
            return Ok(super::HarnessCompletion {
                cache_work,
                built: Err(Error::refused(
                    "the original harness root failed or was cancelled; no complete artifact is published",
                )),
            });
        }
        let built = self.complete(
            ctx,
            tools,
            Ran {
                code: bytes.outcome.code,
                stdout: bytes.stdout,
                stderr: bytes.stderr,
            },
        );
        Ok(super::HarnessCompletion { built, cache_work })
    }
}
