// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stand-in `claude auth login` for the owned capture's tests ([`super::capture_bounded_owned`]):
//! a provider that waits for ever, as Claude Code waits at its paste prompt once nothing answers
//! it, or one that signs in and exits by itself, with a second process left in its group holding
//! its stdout, as an opener closing its browser would, each proven by its pid and its birth.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::process::{Birth, read_identity};

/// One process as a test proves it: its pid and its birth.
#[derive(Clone, Copy, Debug)]
pub(in crate::harness) struct Born {
    pub(in crate::harness) pid: i32,
    birth: Birth,
}

impl Born {
    /// `pid` as it reads now; `None` once it is gone.
    pub(in crate::harness) fn of(pid: i32) -> Option<Self> {
        read_identity(pid).map(|now| Self {
            pid,
            birth: now.birth,
        })
    }

    /// Whether its pid no longer reads its birth: it ended (a zombie reads nothing on macOS), or
    /// the pid names a later process.
    fn gone(self) -> bool {
        read_identity(self.pid).is_none_or(|now| now.birth != self.birth)
    }

    /// Whether its pid still reads its birth.
    pub(in crate::harness) fn runs(self) -> bool {
        !self.gone()
    }

    /// Whether it is gone within the one-minute hang detector: once ended, the reap of a process
    /// whose parent is gone is its adopter's.
    pub(in crate::harness) fn gone_within_a_minute(self) -> bool {
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !self.gone() {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

/// The stand-in in a folder of its own: run as [`Provider::program`], it starts a `sleep` in its
/// group, which inherits its stdout, and says that one's pid (`member`), then its own (`pid`).
/// One made by [`Provider::new`] then waits for ever; at `SIGTERM` it says `termed` and exits,
/// leaving the `sleep` to its group's end. One made by [`Provider::signing_in`] prints
/// [`SIGNED_IN`] and exits 0. Dropped, it `SIGKILL`s whichever of the two still reads the birth
/// it was proven with, so a failing run leaves neither behind.
pub(in crate::harness) struct Provider {
    dir: aterm_tempfile::TempDir,
    said: Arc<Mutex<Option<[Born; 2]>>>,
    /// The member a provider that exited left, as [`Self::member`] proved it.
    left: Mutex<Option<Born>>,
}

/// What a [`Provider::signing_in`] prints before it exits.
pub(in crate::harness) const SIGNED_IN: &[u8] = b"signed in\n";

impl Provider {
    /// The stand-in that waits for ever, laid out for the test `tag`.
    pub(in crate::harness) fn new(tag: &str) -> Self {
        Self::with_tail(tag, "while :; do '{sleep}' 0.1; done\n")
    }

    /// Leaves stdin unread until the test cooperatively releases its input gate.
    pub(in crate::harness) fn unread_input(tag: &str) -> Self {
        Self::with_tail(
            tag,
            "while [ ! -e \"$d/release-input\" ]; do '{sleep}' 0.1; done\nIFS= read -r answer\nexit 0\n",
        )
    }

    /// Releases this stand-in's input gate, including recovery from a failed bounded-write test.
    pub(in crate::harness) fn release_input(&self) {
        std::fs::write(self.dir.path().join("release-input"), b"read\n")
            .expect("release owned input");
    }

    /// The stand-in that signs in and exits by itself, its member still holding its stdout,
    /// laid out for the test `tag`.
    pub(in crate::harness) fn signing_in(tag: &str) -> Self {
        Self::with_tail(tag, "echo signed in\nexit 0\n")
    }

    /// The stand-in whose script ends with `tail`, `{sleep}` in it the kept `sleep`.
    fn with_tail(tag: &str, tail: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-owned-{tag}-"))
            .suffix(".noindex")
            .tempdir()
            .expect("the stand-in's folder");
        let sleep = crate::test_program::kept("/bin/sleep");
        let sleep = sleep.to_str().expect("a UTF-8 sleep path");
        assert!(!sleep.contains('\''), "quoted below: {sleep}");
        let body = format!(
            "#!/bin/sh\nd=${{0%/*}}\ntrap 'echo termed > \"$d/termed\"; exit 0' TERM\n\
             '{sleep}' 3600 &\necho $! > \"$d/member\"\necho $$ > \"$d/pid\"\n{}",
            tail.replace("{sleep}", sleep)
        );
        aterm_tempfile::stand_in(&dir.path().join("claude"), &body).expect("the stand-in");
        Self {
            dir,
            said: Arc::default(),
            left: Mutex::default(),
        }
    }

    /// The stand-in's path.
    pub(in crate::harness) fn program(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    /// A folder beside the stand-in, made on first ask.
    pub(in crate::harness) fn folder(&self, name: &str) -> PathBuf {
        let folder = self.dir.path().join(name);
        std::fs::create_dir_all(&folder).expect("a folder beside the stand-in");
        folder
    }

    /// True once both its processes said their pids and each read as alive, which proves them:
    /// what a capture's settle waits on before it takes its deadline again.
    pub(in crate::harness) fn ready(&self) -> impl Fn() -> bool + Send + 'static {
        let dir = self.dir.path().to_path_buf();
        let said = Arc::clone(&self.said);
        move || {
            let pid = |name: &str| {
                std::fs::read_to_string(dir.join(name))
                    .ok()
                    .filter(|text| text.ends_with('\n'))
                    .and_then(|text| text.trim().parse::<i32>().ok())
            };
            let proven = pid("member")
                .and_then(Born::of)
                .zip(pid("pid").and_then(Born::of));
            if let Some((member, leader)) = proven {
                *said.lock().unwrap_or_else(PoisonError::into_inner) = Some([leader, member]);
            }
            proven.is_some()
        }
    }

    /// The provider and what it left in its group, as [`Self::ready`] proved them.
    pub(in crate::harness) fn said(&self) -> [Born; 2] {
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .expect("the stand-in said its pids before its capture gave up on it")
    }

    /// Whether it said `termed`: a `SIGTERM` reached it.
    pub(in crate::harness) fn termed(&self) -> bool {
        self.dir.path().join("termed").exists()
    }

    /// What the provider left in its group, proven by its pid and its birth, while it still runs
    /// (`None` once it is gone): read once the provider has exited, which it says only after the
    /// member's pid.
    pub(in crate::harness) fn member(&self) -> Option<Born> {
        let born = std::fs::read_to_string(self.dir.path().join("member"))
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse::<i32>().ok())
            .and_then(Born::of)?;
        *self.left.lock().unwrap_or_else(PoisonError::into_inner) = Some(born);
        Some(born)
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        let said = *self.said.lock().unwrap_or_else(PoisonError::into_inner);
        let left = *self.left.lock().unwrap_or_else(PoisonError::into_inner);
        for born in said.into_iter().flatten().chain(left) {
            if !born.gone() {
                // SAFETY: a process this test's stand-in started, its birth read again just now.
                unsafe { libc::kill(born.pid, libc::SIGKILL) };
            }
        }
    }
}
