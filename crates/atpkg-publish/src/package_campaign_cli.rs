// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native regular-package publication entry point. Source building and process
//! adoption remain with the source controller and the signed client protocol.

use super::*;
use crate::artifact::tools::HostTools;
use crate::channel::Log;
use std::collections::BTreeSet;

/// One durable regular-package publication, through Agh for both owners.
pub const SYNOPSIS: &str = "publish-package PKG --build YYYYMMDDNN --source-repo DIR --state DIR [--program codex] [--valid-until YYYY-MM-DDTHH:MM:SSZ] [--prepare-codex-protocol | --expect-codex-protocol SHA256] (requires --host agh)";

/// Qualify, retain, stage, read back and publish one package and its exact index.
/// # Errors
/// Invalid flags, non-Agh hosts, or any native campaign refusal.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut native = Vec::new();
    let mut state = None;
    let mut valid_until = None;
    let mut prepare_protocol = false;
    let mut expected_protocol = None;
    let mut source = false;
    let mut seen = BTreeSet::new();
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--prepare-codex-protocol" if !prepare_protocol => prepare_protocol = true,
            "--expect-codex-protocol" if expected_protocol.is_none() => {
                expected_protocol = Some(arguments.next().ok_or_else(|| Error::usage(SYNOPSIS))?);
            }
            "--state" => {
                let value = arguments.next().ok_or_else(|| Error::usage(SYNOPSIS))?;
                if state.replace(PathBuf::from(value)).is_some() {
                    return Err(Error::usage("publish-package names --state once"));
                }
            }
            "--valid-until" => {
                let value = arguments.next().ok_or_else(|| Error::usage(SYNOPSIS))?;
                if valid_until.replace(value.as_str()).is_some() {
                    return Err(Error::usage("publish-package names --valid-until once"));
                }
            }
            "--out" | "--key" | "--toolchain" | "--upload" => {
                return Err(Error::usage(
                    "publish-package owns its outputs, signing identity, compiler selection and upload stages",
                ));
            }
            "--source-repo" | "--build" | "--program" => {
                if !seen.insert(argument.as_str()) {
                    return Err(Error::usage(format!("publish-package repeats {argument}")));
                }
                source |= argument == "--source-repo";
                native.push(argument.clone());
                native.push(
                    arguments
                        .next()
                        .ok_or_else(|| Error::usage(SYNOPSIS))?
                        .clone(),
                );
            }
            value if value.starts_with('-') => return Err(Error::usage(SYNOPSIS)),
            _ => native.push(argument.clone()),
        }
    }
    let state = state.ok_or_else(|| Error::usage(SYNOPSIS))?;
    if !source {
        return Err(Error::usage(
            "publish-package needs --source-repo explicitly",
        ));
    }
    let request = package::parse_args(&mut ctx.clone(), &native)?;
    if (prepare_protocol || expected_protocol.is_some()) && request.program != "codex"
        || prepare_protocol && (expected_protocol.is_some() || valid_until.is_some())
    {
        return Err(Error::usage(SYNOPSIS));
    }
    if ctx.host_choice() != crate::ctx::HostChoice::Agh {
        return Err(Error::refused(
            "publish-package requires --host agh for journal-owned recovery on both owners",
        ));
    }
    let (_, row, target) = package::package_row(&request.program)?;
    let pkg = package::read_package(&request.package, row.exposes[0], target)?;
    let intent = Intent::from_package(ctx, &request.program, request.build, &pkg)?;
    let mut campaign = Campaign::open(&state, &intent)?;
    let inputs = crate::project_toolchain::Inputs::process(ctx, &request.source_repo);
    let signed_root = |build| atpkg::cli::signed_tree_root("trust", build);
    if prepare_protocol {
        return campaign.prepare_protocol(ctx, &request, &HostTools, &inputs, &signed_root);
    }
    if let Some(hash) = expected_protocol {
        campaign.expect_protocol(hash)?;
    }
    let outcome = campaign.publish_package(
        ctx,
        &request,
        &HostTools,
        &inputs,
        &signed_root,
        valid_until,
        &mut Log::echo("atpkg-publish publish-package"),
    )?;
    if outcome.published_now {
        println!(
            "{} {} build {} published and read back at index {}",
            request.program, pkg.version, request.build, outcome.index_build
        );
    } else {
        println!(
            "{} {} build {} was already published at index {}, read back again: nothing to do",
            request.program, pkg.version, request.build, outcome.index_build
        );
    }
    Ok(())
}
