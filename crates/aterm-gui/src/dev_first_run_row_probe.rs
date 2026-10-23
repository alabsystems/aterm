// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless dev probe for the first-run lane's informational exit row (`first-run-info:<dir>`).

use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

/// Whether `mode` selected this probe, even if its private inputs were refused.
pub(crate) fn start(mode: &OsStr, headless: bool, proxy: EventLoopProxy<Wake>) -> bool {
    let Some(dir) = mode
        .to_str()
        .and_then(|s| s.strip_prefix("first-run-info:"))
    else {
        return false;
    };
    let root = aterm_types::dirs::state_dir().and_then(|p| p.canonicalize().ok());
    let dir = Path::new(dir);
    let dir = dir.is_absolute().then(|| dir.canonicalize()).transpose();
    let allowed = match (root, dir) {
        (Some(root), Ok(Some(dir)))
            if headless
                && aterm_types::dirs::runs_under_private_state_root()
                && dir.is_dir()
                && dir.starts_with(&root)
                && dir != root =>
        {
            Some(dir)
        }
        _ => None,
    };
    let Some(dir) = allowed else {
        aterm_log::warn!(
            "first-run row probe refused: needs headless and a directory beneath the private state root"
        );
        return true;
    };
    let spawned = crate::qos::spawn_lane_thread(
        "first-run-row-probe",
        crate::qos::Role::Background,
        move || {
            let step = dir.join("step");
            if !wait_step(&step, 1) {
                return;
            }
            if proxy
                .send_event(Wake::PkgFirstRunAhead { open: true })
                .is_err()
            {
                return;
            }
            let _ = proxy.send_event(Wake::PkgPassEnded { clean: true });
            if wait_step(&step, 2) {
                let detail = format!(
                    "ALab publishes no build {} (x86_64-apple-darwin) — it builds for \
                     aarch64-apple-darwin; nothing installs here until one is published",
                    atpkg::cli::UNSERVED_ARCHITECTURE_CLAUSE
                );
                let _ = proxy.send_event(Wake::PkgSeedUnusable { detail });
                let _ = proxy.send_event(Wake::PkgFirstRunAhead { open: true });
                let _ = proxy.send_event(Wake::PkgPassEnded { clean: false });
                let _ = wait_step(&step, 3);
            }
            let _ = proxy.send_event(Wake::PkgFirstRunAhead { open: false });
        },
    );
    if let Err(error) = spawned {
        aterm_log::warn!("first-run row probe could not start: {error}");
    }
    true
}

/// Waits up to one minute for the step file at `path` to reach `wanted`.
fn wait_step(path: &Path, wanted: u8) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path);
        if let Ok(mut file) = opened
            && file.metadata().is_ok_and(|m| m.file_type().is_file())
        {
            let mut bytes = [0; 2];
            if file
                .read(&mut bytes)
                .is_ok_and(|n| n > 0 && bytes[0].is_ascii_digit() && bytes[0] - b'0' >= wanted)
            {
                return true;
            }
        }
        if Instant::now() >= deadline {
            aterm_log::warn!("first-run row probe controller did not reach step {wanted}");
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
