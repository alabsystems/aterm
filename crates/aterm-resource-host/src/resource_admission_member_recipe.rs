// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live job capability does not qualify arbitrary executable code. Native
//! compiler members must be the paired maintained artifact; foreign phases
//! require their own future atomic host transition and are refused here.
use super::*;
use aterm_types::resource_subgroup::{GnuTransport, Request};
use std::os::unix::ffi::OsStrExt as _;

pub(super) fn command(
    job: &Job,
    input: &Request,
    timeout: Option<Duration>,
) -> io::Result<spawnfd::Command> {
    let request = &input.member;
    compiler::no_loader_overrides(&request.env)?;
    if !job.native_targo {
        return Err(refused("campaign subgroup recipe is not qualified"));
    }
    let compiler = job
        .compiler
        .as_ref()
        .ok_or_else(|| refused("original project compiler lease unavailable"))?;
    let target = compiler.execution().join("bin/trustc");
    if recipe::resolve(request)? != target || target.parent() != job.program.parent() {
        return Err(refused(
            "native compiler is not paired with the admitted project driver",
        ));
    }
    let words = match &request.argv[1..] {
        [worker, subgroups, rest @ ..]
            if worker == b"--resource-worker-v3" && subgroups == b"--resource-subgroups-v3" =>
        {
            rest
        }
        [first, ..] if first == b"--resource-worker-v3" || first == b"--resource-subgroups-v3" => {
            return Err(refused("incomplete native member execution markers"));
        }
        words => words,
    };
    no_early_execution(words)?;
    let metadata = metadata(words, &input.gnu, timeout)?;
    if !metadata && matches!(input.gnu, GnuTransport::None) {
        return Err(refused(
            "native compilation requires the original shared GNU context",
        ));
    }
    let mut arguments = words
        .iter()
        .map(|word| std::ffi::OsStr::from_bytes(word))
        .collect::<Vec<_>>();
    if !metadata {
        // The coupled v3 driver advertises only with a compiler that enforces
        // these markers before parsing ordinary compiler options. An older
        // replacement refuses them, before any compilation can execute.
        arguments.insert(0, std::ffi::OsStr::new("--resource-subgroups-v3"));
        arguments.insert(0, std::ffi::OsStr::new("--resource-worker-v3"));
    }
    let mut command = spawnfd::command(&target);
    command
        .arg0(std::ffi::OsStr::from_bytes(&request.argv[0]))
        .args(arguments)
        .env_clear()
        .envs(
            request
                .env
                .iter()
                .filter(|(name, value)| !recipe::stale_fd_var(name, value))
                .map(|(name, value)| {
                    (
                        std::ffi::OsStr::from_bytes(name),
                        std::ffi::OsStr::from_bytes(value),
                    )
                }),
        )
        .env("ATERM_RESOURCE_JOB_V3", job.delegation.encode()?)
        .env(
            "ATERM_RESOURCE_INSPECTOR",
            crate::runtime::inspector_program()?,
        )
        .env("ATERM_SESSION_ID", &job.binding.session)
        .env("ATERM_PARENT_SESSION_ID", &job.binding.session)
        .current_dir(std::ffi::OsStr::from_bytes(&request.cwd))
        .start_suspended(true);
    Ok(command)
}

/// These probes consume an already borrowed CPU credit; they create no grant.
fn metadata(words: &[Vec<u8>], gnu: &GnuTransport, timeout: Option<Duration>) -> io::Result<bool> {
    if matches!(words, [word] if word == b"-vV" || word == b"--version") {
        return Ok(true);
    }
    let probe =
        matches!(words, [word] if word == b"--print=sysroot" || word == b"--print=target-list");
    if probe
        && (!matches!(gnu, GnuTransport::None)
            || timeout.is_none_or(|limit| limit.is_zero() || limit > Duration::from_secs(8)))
    {
        return Err(refused(
            "compiler metadata probe requires a bounded isolated request",
        ));
    }
    Ok(probe)
}

#[cfg(test)]
#[path = "resource_admission_member_recipe_tests.rs"]
mod tests;

/// The compiler handles explanation before entering its authenticated Session.
/// Its PAGER and opaque argument-file expansion are outside this native recipe.
fn no_early_execution(words: &[Vec<u8>]) -> io::Result<()> {
    if words.iter().any(|word| {
        word == b"--explain" || word.starts_with(b"--explain=") || word.first() == Some(&b'@')
    }) {
        return Err(refused(
            "native compiler member cannot execute an early pager or opaque argfile",
        ));
    }
    Ok(())
}
