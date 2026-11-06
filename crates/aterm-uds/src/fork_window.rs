// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support: a child parked between `fork` and `exec` ([`Parked`]), the state in which a
//! descriptor this process closes stays open in the child.

use std::io::{Read as _, Write as _};

const SHELL: &str = crate::os_path!("sh");

const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// A child parked between `fork` and `exec`, holding a copy of every descriptor this
/// process had open: the state a sibling test's `pre_exec` spawn puts this process in for
/// milliseconds at a time, made deterministic. An flock lives on the open file description, so
/// a lock let go by closing its descriptor stays held while this child lives; one let go by
/// `LOCK_UN` does not.
pub struct Parked {
    ours: std::os::unix::net::UnixStream,
    spawner: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
}

impl Parked {
    /// Fork a child and return once it is parked. The child writes one byte, then waits
    /// for one, under the hang detector on the shared socket, so even a dead parent cannot
    /// strand it in `pre_exec`. A child never let go fails its spawn instead of its exec,
    /// which would close its copies under a probe still to come and pass it unheld.
    ///
    /// # Panics
    /// The socket pair could not be made or read, or the child never reported parking.
    #[must_use = "the child is let go when this is released or dropped"]
    pub fn park() -> Self {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
        theirs.set_read_timeout(Some(HANG)).expect("timeout");
        let theirs_fd = theirs.as_raw_fd();
        let spawner = std::thread::spawn(move || {
            #[expect(
                clippy::disallowed_types,
                reason = "the parked child must fork holding every descriptor and run `pre_exec`, \
                          which spawnfd's children never do"
            )]
            let mut cmd = std::process::Command::new(SHELL);
            cmd.args(["-c", "exit 0"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // SAFETY: runs in the forked child before `exec` and calls only the
            // async-signal-safe `write(2)` and `read(2)`, on a descriptor the parent keeps open
            // (`theirs`, alive until `status` returns); its error is a raw code, no allocation.
            unsafe {
                cmd.pre_exec(move || {
                    let mut byte = 0u8;
                    libc::write(theirs_fd, (&raw const byte).cast(), 1);
                    if libc::read(theirs_fd, (&raw mut byte).cast(), 1) == 1 {
                        Ok(())
                    } else {
                        Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT))
                    }
                });
            }
            let status = cmd.status();
            drop(theirs);
            status
        });
        let mut parked = Self {
            ours,
            spawner: Some(spawner),
        };
        let mut byte = [0u8];
        parked
            .ours
            .read_exact(&mut byte)
            .expect("the child parked between fork and exec");
        parked
    }

    /// Let the child exec and exit, and return how it exited.
    ///
    /// # Panics
    /// The spawner thread panicked, or the spawn failed: the exec, or a park held past the
    /// hang detector.
    pub fn release(mut self) -> std::process::ExitStatus {
        self.let_go();
        let spawner = self.spawner.take().expect("released once");
        match spawner.join().expect("the spawner thread") {
            Ok(status) => status,
            Err(e) => panic!(
                "spawning `{SHELL} -c 'exit 0'` for the parked child failed: {e} \
                 (the child forks and parks before its exec, so this is the exec, or a \
                 park the test held past the hang detector)"
            ),
        }
    }

    fn let_go(&mut self) {
        let _ = self.ours.write_all(b"g");
        let _ = self.ours.shutdown(std::net::Shutdown::Write);
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        if let Some(spawner) = self.spawner.take() {
            self.let_go();
            let _ = spawner.join();
        }
    }
}
