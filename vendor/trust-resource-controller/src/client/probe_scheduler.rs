// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Early GNU/SDK custody for fixed trusted synchronous compiler queries.
//! No admission or extra credit is created. The callback must retire its child
//! before returning, including capture-error paths; its serial guard stays live.
use std::io;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::sync::Mutex;

use jobserver_utils::Jobserver;

use super::sdk;

pub struct ProbeScheduler {
    server: Mutex<Option<Jobserver>>,
}
impl ProbeScheduler {
    /// Capture before any descriptor-opening operation or arbitrary callback.
    /// # Safety
    /// Only the SDK snapshot and fixed GNU importer may precede SDK capture.
    pub unsafe fn capture_at_entry() -> io::Result<Self> {
        let pending = sdk::snapshot_sdk_lease()?;
        // SAFETY: our caller guarantees genuine native entry and original FDs.
        let server = unsafe { Jobserver::from_env_with_handoff_checked() }?;
        // SAFETY: only the fixed scheduler importer followed the snapshot.
        unsafe { sdk::capture_sdk_lease(pending) }?;
        if server.is_none() && std::env::var_os(jobserver_utils::DESCENDANT_HANDOFF_ENV).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "advertised probe scheduler has no inherited transport",
            ));
        }
        Ok(Self { server: Mutex::new(server) })
    }

    /// Configure only the caller's fixed trusted compiler query. GNU variables
    /// describe our real owned transport. No jobs/environment integer is credit.
    /// The original SDK/CPU descriptors are passed through without reopening or
    /// reconstructing a lease. A locator still confers no authentication.
    pub fn with_command<T>(
        &self,
        command: &mut Command,
        run: impl FnOnce(&mut Command) -> T,
    ) -> io::Result<T> {
        let mut guard =
            self.server.lock().map_err(|_| io::Error::other("poisoned serial probe owner"))?;
        if guard.is_none() {
            // ZERO extras; the synchronous child borrows this owner's implicit execution.
            *guard = Some(Jobserver::new(0)?);
        }
        let server = guard.as_mut().expect("initialized above");
        server.enable_descendant_handoff()?;
        server.configure_make(command);
        let descriptions =
            server.pipe_descriptors().map(|pipe| [pipe[0].as_raw_fd(), pipe[1].as_raw_fd()]);
        if let Some(descriptions) = descriptions {
            super::member::inherit_descriptions(command, &descriptions)?;
        }
        sdk::inherit_original_lease(command)?;
        // Guard and real scheduler survive the complete existing capture/reap.
        Ok(run(command))
    }
}
