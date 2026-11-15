// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real constructor admission for the explicit Rust-only protocol fixture.
//! Worker UI typing remains a separate qualification; no image override exists.

use aterm_agent::harness::{
    account_work,
    cli::Env,
    codex_enrollment::SourceProofLaunch,
    codex_source_registry::{OwnedLaunchEnvironment, Registry},
    worker,
};
use aterm_uds::CtlStream;
use aterm_uds::spawnfd::{BirthAttempt, Child};
use std::sync::Arc;

pub(crate) fn spawn(
    env: &Env,
    sid: &str,
    registry: Arc<Registry>,
    scope: account_work::Scope,
    launch: &SourceProofLaunch,
    environment: &[(String, String)],
) -> Result<(Child, CtlStream), String> {
    launch.current()?;
    if launch.retained_frontend() {
        return Err("source fixture requires its authenticated purpose".into());
    }
    let codex_home = env
        .home
        .as_ref()
        .ok_or("fixture HOME missing")?
        .join(".codex");
    // An explicit CODEX_HOME must already exist when the normal SDK config
    // loader canonicalizes it. Keep that prerequisite inside this private world.
    aterm_secrets::fsx::ensure_private_dir(&codex_home)
        .map_err(|_| "source proof CODEX_HOME unavailable")?;
    let cli = worker::worker_cli(aterm_phase::Program::Codex).ok_or("Codex worker unavailable")?;
    let process = aterm_agent::harness::native_act::Process::observe(std::process::id())
        .ok_or("source proof environment process unavailable")?;
    let captured = OwnedLaunchEnvironment::from_snapshot(
        &process,
        environment
            .iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
    )
    .map_err(|_| "source proof explicit environment unavailable")?;
    let mut values = environment
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    values.insert(
        "CODEX_HOME".into(),
        codex_home.to_string_lossy().into_owned(),
    );
    values.insert(
        "CODEX_AUTH_PATH".into(),
        launch.auth_file().to_string_lossy().into_owned(),
    );
    let starter = captured
        .prepare(worker::Starter {
            exe: launch.candidate().to_path_buf(),
            args: Ok(vec![
                "alab-native-source-proof".into(),
                "--owned-control".into(),
            ]),
            env: values.into_iter().collect(),
            unset: vec!["CODEX_AUTH_SLOT"],
        })
        .map_err(|_| "source proof environment admission refused")?;
    let admitted = worker::admit(
        cli,
        starter,
        |_| launch.current().is_ok(),
        |_| None,
        |_| Err("isolated source proof never installs a program".into()),
    )
    .map_err(|_| "source proof worker admission refused")?;
    let reservation = registry
        .reserve_owned_worker(env, sid, &admitted, &scope, captured)
        .map_err(|_| "source proof owned constructor refused")?;
    let admitted = reservation
        .arm(admitted)
        .map_err(|_| "source proof owned constructor could not arm")?;
    launch.current()?;
    let attempt = BirthAttempt::new().map_err(|_| "source proof birth attempt unavailable")?;
    let (mut worker, control) = match reservation.spawn_with_control(admitted, &env.cwd, attempt) {
        Ok(started) => started,
        Err(mut failure) => {
            loop {
                match failure.stop() {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(_) => return Err("source proof failed birth cleanup unresolved".into()),
                }
            }
            return Err("source proof actual owned spawn refused".into());
        }
    };
    // No guardian may write its JSON command stream until the SDK has consumed
    // the physical challenge and native code observes its exact connection ACK.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match worker.check_control_binding() {
            Ok(()) => break,
            Err(aterm_agent::harness::codex_source_registry::Error::Busy)
                if std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(_) => return Err("source proof actual socket binding unavailable".into()),
        }
    }
    launch.current()?;
    // This leaf-only fixture transfers the genuine child to its existing owning
    // guard; production backend supervision retains the complete OwnedWorker.
    let child = worker
        .into_spawned()
        .map_err(|_| "source proof actual child transfer unavailable")?
        .into_child();
    Ok((child, control))
}
