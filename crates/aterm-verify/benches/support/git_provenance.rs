// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Query the recorded SDK executable, never infer it from the setup Git resolver.

use agh::Github;
use aterm_json::json;
use std::error::Error;
use std::time::{Duration, Instant};

pub(crate) fn recorded_git(client: &Github) -> Result<(aterm_json::Value, bool), Box<dyn Error>> {
    let children = client.fx().children_run();
    let first = children.first().ok_or("runner recorded no Git child")?;
    let same_program = children.iter().all(|child| child.program == first.program);
    let mut command = aterm_uds::spawnfd::command(&first.program);
    command
        .env_clear()
        .envs(first.env.iter().cloned())
        .arg("--version");
    let version = aterm_uds::deadline::output_until_complete_stdout(
        &mut command,
        Instant::now() + Duration::from_secs(60),
    )?;
    let version_text = String::from_utf8(version.stdout)?;
    let version_text = version_text.trim();
    let version_valid = version.status.success()
        && version_text.starts_with("git version ")
        && version_text.lines().count() == 1
        && version_text.len() <= 128;
    let actual_git = json!({"version":if version_valid {Some(version_text)} else {None},
    "recorded_children":children.len(),"same_program":same_program,
    "version_probe_valid":version_valid,"probe_outside_timing":true});
    Ok((actual_git, same_program && version_valid))
}
