// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two macOS guests per account, across checkouts and lab verbs. The boot process inherits
//! the locked description: a dead lab cannot free a slot while its Tart process still runs.
//! No clock, PID reuse, or old heartbeat can evict a live holder.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::Instant;

use aterm_toolchain::record_lock::{CloseOnlyLock, RecordLock};

use crate::{Fail, stop};

pub(crate) struct Slot(CloseOnlyLock);

impl Slot {
    #[cfg(test)]
    fn try_in(dir: &Path) -> Result<Option<Self>, Fail> {
        Self::try_names(dir, &["guest-a", "guest-b"])
    }

    fn try_names(dir: &Path, names: &[&str]) -> Result<Option<Self>, Fail> {
        for name in names {
            let path = dir.join(name);
            // The existing regular-file helper refuses links and FIFOs, and keeps CLOEXEC.
            let file = crate::util::append_regular_file(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            match RecordLock::try_lock(file, 0) {
                Ok(lock) => return Ok(Some(Self(lock.into_sole_close_only()))),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(e) => return Err(format!("{}: {e}", path.display()).into()),
            }
        }
        Ok(None)
    }

    pub(crate) fn take() -> Result<Self, Fail> {
        Self::take_names(&["guest-a", "guest-b"])
    }

    pub(crate) fn display() -> Result<Self, Fail> {
        Self::take_names(&["guest-display"])
    }

    fn take_names(names: &[&str]) -> Result<Self, Fail> {
        let home = aterm_uds::account::home()
            .ok_or_else(|| Fail::die("no account home for macOS guest slots"))?;
        let dir = home.join("Library/Caches/aterm/mac-guests");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut said = Instant::now();
        loop {
            stop::check()?;
            if let Some(slot) = Self::try_names(&dir, names)? {
                return Ok(slot);
            }
            if said.elapsed().as_secs() >= 60 {
                println!("mac: guest slots {names:?} held; waiting for a guest to stop");
                said = Instant::now();
            }
            stop::sleep(crate::mac::POLL)?;
        }
    }

    pub(crate) fn inherited(&self) -> Result<OwnedFd, Fail> {
        self.0
            .try_clone_fd()
            .map_err(|e| format!("mac guest slot: {e}").into())
    }
}

#[cfg(test)]
#[path = "mac_slots_tests.rs"]
mod tests;
