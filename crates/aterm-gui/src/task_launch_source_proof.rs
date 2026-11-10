// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real constructor admission for the explicit Rust-only protocol fixture.
//! Worker UI typing remains a separate qualification; no image override exists.

use super::codex_source::Preparation;
use aterm_agent::harness::{
    account_work, cli::Env, codex_enrollment::SourceProofLaunch, codex_source_registry::Registry,
    worker,
};
use aterm_uds::spawnfd::{Child, Stdio, Tether};
use std::sync::Arc;

pub(crate) fn spawn(
    env: &Env,
    sid: &str,
    registry: Arc<Registry>,
    scope: account_work::Scope,
    launch: &SourceProofLaunch,
    environment: &[(String, String)],
) -> Result<Child, String> {
    launch.current()?;
    let cli = worker::worker_cli(aterm_phase::Program::Codex).ok_or("Codex worker unavailable")?;
    let starter = worker::Starter {
        exe: launch.candidate().to_path_buf(),
        args: Ok(vec!["alab-native-source-proof".into()]),
        env: vec![
            (
                "CODEX_HOME".into(),
                env.home
                    .as_ref()
                    .ok_or("fixture HOME missing")?
                    .join(".codex")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "CODEX_AUTH_PATH".into(),
                launch.auth_file().to_string_lossy().into_owned(),
            ),
        ],
        unset: vec!["CODEX_AUTH_SLOT"],
    };
    let admitted = worker::admit(
        cli,
        starter,
        env.home.as_deref(),
        |_| launch.current().is_ok(),
        |_| None,
        |_| Err("isolated source proof never installs a program".into()),
    )
    .map_err(|_| "source proof worker admission refused")?;
    let mut source = Preparation::with_context(registry, env.clone(), sid.into(), scope)?;
    let admitted = source.arm(admitted, cli)?;
    let (admitted, ()) = admitted
        .prepare(cli, env.home.as_deref(), |starter| Ok((starter, ())))
        .map_err(|_| "source proof preparation refused")?;
    launch.current()?;
    source.check(&admitted)?;
    let starter = admitted.starter();
    let mut command = aterm_uds::spawnfd::command(&starter.exe);
    #[cfg(target_os = "macos")]
    if aterm_uds::tether::executable().is_none() {
        return Err("source proof host has no live parent-death tether".into());
    }
    command
        .env_clear()
        .envs(environment.iter().cloned())
        .envs(starter.env.iter().cloned())
        .args(
            starter
                .args
                .as_ref()
                .map_err(|_| "source proof argv unavailable")?,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .tether();
    for name in &starter.unset {
        command.env_remove(name);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "source proof child could not start")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let mapped =
            aterm_agent::harness::native_act::Process::observe(child.id()).is_some_and(|process| {
                Some(process.image)
                    == aterm_agent::harness::native_act::Image::at(launch.candidate())
            });
        if mapped {
            break;
        }
        if child
            .try_wait()
            .map_err(|_| "source proof child wait failed")?
            .is_some()
            || std::time::Instant::now() >= deadline
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("source proof candidate did not reach its mapped image".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    if source.spawned(child.id()).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("source proof kernel child admission refused".into());
    }
    Ok(child)
}
