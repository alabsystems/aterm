// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS seam: the bin serving on unix sockets with an inert `System`, so the
//! host's tests drive the real executable end to end. Never a shipped mode; nothing here
//! changes this Mac (no clock, no mount, no power-off, no identity switch).

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::system::{Booted, Facts, Identity, RawJob, System};
use crate::wire::Share;

/// A guest that is already up and changes nothing.
pub struct Seam {
    started: Instant,
}

impl Default for Seam {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl System for Seam {
    fn facts(&self) -> Facts {
        Facts {
            boot_id: format!("seam-{}", std::process::id()),
            kernel: std::env::consts::OS.to_string(),
        }
    }

    fn uptime_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn jobs(&self) -> Vec<RawJob> {
        Vec::new()
    }

    fn booted(&self) -> Booted {
        Booted {
            state: "running".into(),
            failed_units: Vec::new(),
        }
    }

    fn user_bus(&self) -> Result<(), String> {
        Ok(())
    }

    fn now_unix_ns(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }

    fn set_clock(&self, _unix_ns: u64) -> Result<(), String> {
        Err("the seam never sets this Mac's clock".into())
    }

    fn mount(&self, _shares: &[Share]) -> Result<(), String> {
        Err("Mount runs only in the Linux guest".into())
    }

    fn poweroff(&self) -> Result<(), String> {
        Err("Poweroff runs only in the Linux guest".into())
    }

    fn identity(&self) -> Result<Identity, String> {
        Ok(Identity::Keep)
    }
}

/// Serve commands on the unix socket `listen` (bound before the dial, as the guest binds
/// its vsock port) and speak the watch connection to the unix socket `watch`, its `Hello`
/// carrying the key the key disk file `key_disk` holds (as the guest reads its device).
#[must_use]
pub fn run(listen: &Path, watch: &Path, key_disk: &Path, tick: Duration) -> ExitCode {
    let say = |what: String| eprintln!("aterm-vz-agent (seam): {what}");
    let listener = match UnixListener::bind(listen) {
        Ok(l) => l,
        Err(e) => {
            say(format!("could not listen on {}: {e}", listen.display()));
            return ExitCode::FAILURE;
        }
    };
    let boot_key = match crate::key::read_disk(key_disk) {
        Ok(Some(k)) => k,
        Ok(None) => {
            say(format!("{} is no key disk", key_disk.display()));
            return ExitCode::FAILURE;
        }
        Err(e) => {
            say(format!("{}: {e}", key_disk.display()));
            return ExitCode::FAILURE;
        }
    };
    let conn = match UnixStream::connect(watch) {
        Ok(c) => c,
        Err(e) => {
            say(format!("could not dial {}: {e}", watch.display()));
            return ExitCode::FAILURE;
        }
    };
    let system: Arc<dyn System> = Arc::new(Seam::default());
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            say(format!("could not name its own executable: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let hello = match crate::hello(system.as_ref(), &exe, boot_key) {
        Ok(h) => h,
        Err(e) => {
            say(format!("could not hash {}: {e}", exe.display()));
            return ExitCode::FAILURE;
        }
    };
    crate::serve_with_watch(&listener, conn, &system, &hello, tick)
}
