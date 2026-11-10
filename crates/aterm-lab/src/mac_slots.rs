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
mod tests {
    use super::*;

    fn after_close(dir: &Path, names: &[&str]) -> Slot {
        // CLOEXEC descriptions may survive a sibling fork until its exec. Observe the
        // final close instead of imposing a scheduling budget on an unrelated child.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the final guest-slot description to close",
            || Slot::try_names(dir, names).unwrap(),
        )
    }

    #[test]
    fn only_one_guest_display_is_open_beside_two_guest_slots() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let display = Slot::try_names(dir.path(), &["guest-display"])
            .unwrap()
            .unwrap();
        assert!(
            Slot::try_names(dir.path(), &["guest-display"])
                .unwrap()
                .is_none()
        );
        let a = Slot::try_in(dir.path()).unwrap().unwrap();
        let b = Slot::try_in(dir.path()).unwrap().unwrap();
        assert!(Slot::try_in(dir.path()).unwrap().is_none());
        drop(display);
        drop(after_close(dir.path(), &["guest-display"]));
        drop((a, b));
    }

    #[test]
    fn two_guests_hold_both_slots_and_a_live_child_copy_keeps_its_slot() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let a = Slot::try_in(dir.path()).unwrap().unwrap();
        let b = Slot::try_in(dir.path()).unwrap().unwrap();
        assert!(Slot::try_in(dir.path()).unwrap().is_none());
        // NEGATIVE CONTROL: the parent's drop alone must not admit a third guest.
        let child = std::fs::File::from(a.inherited().unwrap());
        drop(a);
        assert!(Slot::try_in(dir.path()).unwrap().is_none());
        drop(child);
        let c = after_close(dir.path(), &["guest-a", "guest-b"]);
        assert!(Slot::try_in(dir.path()).unwrap().is_none());
        drop((b, c));
        drop(after_close(dir.path(), &["guest-a", "guest-b"]));
    }

    #[test]
    fn the_boot_process_keeps_admission_after_its_parent_guard_closes() {
        use aterm_uds::spawnfd::{Stdio, command};
        let dir = aterm_tempfile::tempdir().unwrap();
        let a = Slot::try_in(dir.path()).unwrap().unwrap();
        let b = Slot::try_in(dir.path()).unwrap().unwrap();
        let mut cmd = command("/bin/cat");
        cmd.inherit_guards(vec![a.inherited().unwrap()]).unwrap();
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        drop(cmd);
        drop(a);
        // Removing inherit_guards above admits a third guest here (negative control).
        assert!(Slot::try_in(dir.path()).unwrap().is_none());
        drop(input);
        assert!(child.wait().unwrap().success());
        drop(after_close(dir.path(), &["guest-a", "guest-b"]));
        drop(b);
    }
}
