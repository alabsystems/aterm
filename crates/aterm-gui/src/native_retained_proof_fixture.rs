// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual foreground SDK and independently owned backend qualification only.
use super::*;

pub(super) fn install(
    ctx: &mut crate::SessionCtx,
    launch: &SourceProofLaunch,
) -> Result<(), String> {
    use crate::native_session_policy::{FixtureImages, FixturePolicy};
    use aterm_agent::harness::{contract, native_act::Constructor};
    // Empty harness scope: this fixture permits a constructor, never a claim,
    // Source, Ready, completion flag or provider permission.
    const ROWS: &[contract::Row] = &[contract::Row {
        id: "codex-retained-proof",
        program: aterm_phase::Program::Codex,
        package: "codex",
        vendor: "codex",
        allows: &[],
    }];
    launch.current()?;
    let policy = FixturePolicy::new(
        ROWS,
        vec![FixtureImages {
            row: ROWS[0].id,
            images: launch.retained_images()?,
        }],
        aterm_agent::supervise::SupervisorConfig {
            headless: true,
            ..Default::default()
        },
        &[Constructor::PrivateRestart],
    )
    .map_err(str::to_owned)?;
    ctx.install_fixture_policy(policy)
        .map_err(|_| "retained proof policy already registered".to_owned())
}

pub(super) fn run(
    ctx: &Arc<crate::SessionCtx>,
    launch: &SourceProofLaunch,
    pty: &Pty,
    sid: &str,
    server: &std::thread::JoinHandle<io::Result<()>>,
    mut pump: impl FnMut(),
) -> Result<Vec<Process>, String> {
    use std::os::fd::BorrowedFd;
    let process =
        Process::observe(pty.shell.pid as u32).ok_or("retained proof frontend unavailable")?;
    let guardian =
        CtlStream::connect(launch.guardian()).map_err(|_| "retained proof guardian unavailable")?;
    let peer = aterm_uds::fdpass::peer_pid(&guardian)
        .map_err(|_| "retained proof guardian identity unavailable")?;
    if !launch.guardian_peer_current(peer) {
        return Err("retained proof guardian changed".into());
    }
    let header = aterm_json::to_vec(&aterm_json::json!({"schema":1,"sid":sid,"hostPid":std::process::id(),"pid":process.pid,"birth":process.birth,"retained":true}))
        .map_err(|_| "retained proof header unavailable")?;
    // SAFETY: this actual fixture retains the owning PTY for the entire call.
    // Passing IO duplicates to its authenticated guardian is not foreground evidence.
    let master = unsafe { BorrowedFd::borrow_raw(pty.master) };
    if aterm_uds::fdpass::send_with_fds(&guardian, &header, &[master, master])
        .map_err(|_| "retained proof descriptor transfer failed")?
        != header.len()
    {
        return Err("retained proof descriptor transfer incomplete".into());
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        pump();
        if server.is_finished() {
            return Err("retained proof control server ended".into());
        }
        if let Some(status) = aterm_pty::collect_exit_status(pty.shell.pid) {
            if !status.is_success() {
                return Err("retained SDK frontend failed".into());
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("retained SDK frontend hang detector".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // The frontend actually exited. Observe the native owner and genuine SDK
    // construction, not a child-reported PID or a saved startup-state string.
    let expected_images = launch.retained_images()?;
    let backends = loop {
        match ctx.native_policy.observe_retained_backends() {
            Ok(backends) if backend_images_match(&backends, &expected_images, process.pid) => {
                eprintln!(
                    "Retained qualification native: backend survives frontend=true sdk_current=true"
                );
                eprintln!(
                    "Retained qualification native: exact backend images={} sdk_current=true",
                    backends.len()
                );
                break backends;
            }
            Ok(_) => {
                return Err("retained backend image set differs from authenticated launch".into());
            }
            Err(aterm_agent::harness::codex_source_registry::Error::Busy)
                if Instant::now() < deadline =>
            {
                pump()
            }
            Err(_) => return Err("retained backend did not survive frontend".into()),
        }
    };
    drop(guardian);
    Ok(backends)
}

/// Compare only independently observed native processes and authenticated image
/// leases. SDK checkpoint identifiers and child-reported paths are not evidence.
fn backend_images_match(
    backends: &[Process],
    images: &[aterm_agent::harness::native_act::Image],
    frontend: u32,
) -> bool {
    !images.is_empty()
        && backends.len() == images.len()
        && backends.iter().enumerate().all(|(index, backend)| {
            backend.pid != frontend
                && !backends[..index]
                    .iter()
                    .any(|prior| prior.pid == backend.pid)
        })
        && images.iter().enumerate().all(|(index, image)| {
            !images[..index].contains(image)
                && backends
                    .iter()
                    .filter(|backend| &backend.image == image)
                    .count()
                    == 1
        })
}
