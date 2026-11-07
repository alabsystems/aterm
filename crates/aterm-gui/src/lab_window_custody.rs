// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-lifetime custody of the private lab's original window reservation.

use std::ffi::OsString;
use std::os::fd::{AsRawFd as _, OwnedFd};

struct Custody {
    lane: OwnedFd,
    memory: Option<(OwnedFd, String)>,
}

static HELD: std::sync::OnceLock<Custody> = std::sync::OnceLock::new();

/// Retain only already-validated, close-only descriptions from the earliest entry.
/// No argument or environment marker creates a reservation here.
/// # Errors
/// A descriptor cannot be duplicated, a claim has no filename, or custody was installed twice.
pub fn retain(
    lane: &aterm_toolchain::record_lock::CloseOnlyLock,
    memory: Option<&aterm_capacity::RetainedClaim>,
) -> Result<(), String> {
    let lane = lane.try_clone_fd().map_err(|e| e.to_string())?;
    let memory = memory
        .map(|held| {
            let leaf = held
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("lab memory custody has no claim leaf")?
                .to_owned();
            Ok::<_, String>((held.try_clone_fd().map_err(|e| e.to_string())?, leaf))
        })
        .transpose()?;
    HELD.set(Custody { lane, memory })
        .map_err(|_| "lab window custody was installed twice".into())
}

pub(crate) fn descriptors() -> Vec<i32> {
    let Some(held) = HELD.get() else {
        return Vec::new();
    };
    let mut fds = vec![held.lane.as_raw_fd()];
    if let Some((memory, _)) = &held.memory {
        fds.push(memory.as_raw_fd());
    }
    fds
}

#[cfg(target_os = "macos")]
pub(crate) fn present() -> bool {
    HELD.get().is_some()
}

fn replace_offer(
    mut args: Vec<OsString>,
    offer: Option<aterm_types::lab_window_guard::GuardBundle>,
) -> Vec<OsString> {
    // These are this process's original arguments, already validated at its earliest entry.
    aterm_types::lab_window_guard::take(&mut args).expect("validated lab window arguments");
    let args = aterm_update::window_successor_args(args.into_iter());
    if let Some(offer) = offer {
        let mut prefix = vec![
            aterm_types::lab_window_guard::FLAG.into(),
            offer.lane.to_string().into(),
        ];
        if let Some(memory) = offer.memory {
            prefix.extend([
                aterm_types::lab_window_guard::MEMORY_FLAG.into(),
                memory.descriptor.to_string().into(),
                memory.claim.into(),
            ]);
        }
        prefix.extend(args);
        prefix
    } else {
        args
    }
}

pub(crate) fn successor_args(args: Vec<OsString>) -> Vec<OsString> {
    let offer = HELD
        .get()
        .map(|held| aterm_types::lab_window_guard::GuardBundle {
            lane: held.lane.as_raw_fd(),
            memory: held.memory.as_ref().map(|(fd, claim)| {
                aterm_types::lab_window_guard::MemoryRef {
                    descriptor: fd.as_raw_fd(),
                    claim: claim.clone(),
                }
            }),
        });
    replace_offer(args, offer)
}

/// Temporarily clear CLOEXEC for an in-place replacement, restoring it on exec failure.
pub(crate) fn inherit_for_exec() -> std::io::Result<Inherit> {
    inherit_descriptors(descriptors())
}

fn inherit_descriptors(descriptors: Vec<i32>) -> std::io::Result<Inherit> {
    let mut inherited = Inherit(Vec::new());
    for fd in descriptors {
        // SAFETY: process-lifetime custody keeps each exact descriptor open. Only its
        // inheritance flag changes; unrelated spawn paths strip inherited descriptors.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        inherited.0.push((fd, flags));
    }
    Ok(inherited)
}

pub(crate) struct Inherit(Vec<(i32, i32)>);
impl Drop for Inherit {
    fn drop(&mut self) {
        for (fd, flags) in &self.0 {
            // SAFETY: the process-lifetime description remains open after exec failed.
            unsafe {
                libc::fcntl(*fd, libc::F_SETFD, *flags);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_exec_and_partial_setup_restore_inheritance_flags() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("guard")).unwrap();
        let fd = file.as_raw_fd();
        // SAFETY: file owns this descriptor throughout this test.
        let flags = || unsafe { libc::fcntl(fd, libc::F_GETFD) };
        let original = flags();
        assert_ne!(original & libc::FD_CLOEXEC, 0);
        {
            let _inherit = inherit_descriptors(vec![fd]).unwrap();
            assert_eq!(flags() & libc::FD_CLOEXEC, 0);
        }
        assert_eq!(flags(), original);
        assert!(inherit_descriptors(vec![fd, -1]).is_err());
        assert_eq!(flags(), original);
    }

    #[test]
    fn successor_replaces_consumed_numbers_and_preserves_payload() {
        let args = [
            "--window",
            "--lab-window-guard",
            "7",
            "--lab-window-memory-guard",
            "8",
            "old.claim",
            "-e",
            "tool",
            "--lab-window-guard",
            "99",
        ]
        .map(OsString::from)
        .to_vec();
        let offer = aterm_types::lab_window_guard::GuardBundle {
            lane: 31,
            memory: Some(aterm_types::lab_window_guard::MemoryRef {
                descriptor: 32,
                claim: "original.claim".into(),
            }),
        };
        let mut replaced = replace_offer(args.clone(), Some(offer.clone()));
        assert_eq!(
            replace_offer(replaced.clone(), Some(offer.clone())),
            replaced
        );
        assert_eq!(
            aterm_types::lab_window_guard::take(&mut replaced).unwrap(),
            Some(offer)
        );
        assert_eq!(
            replaced,
            ["--window", "-e", "tool", "--lab-window-guard", "99"].map(OsString::from)
        );
        assert_eq!(replace_offer(args, None), replaced);
    }
}
