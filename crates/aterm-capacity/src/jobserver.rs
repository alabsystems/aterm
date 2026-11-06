// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One GNU FIFO jobserver for a release's branches. Each client reserves its implicit
//! token before spawning; Cargo and rustc borrow every additional token from this FIFO.

use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// A release-local pool, kept alive until all branch guards have been returned.
#[derive(Debug)]
pub struct Jobserver {
    path: PathBuf,
    file: File,
    slots: u32,
}

impl Jobserver {
    /// Create a new pool in a private release directory. Existing paths are never replaced.
    /// # Errors
    /// The token count or FIFO path cannot be represented, or creating/opening it fails.
    pub fn new(path: &Path, slots: u32) -> io::Result<Self> {
        if slots == 0
            || slots > 4096
            || path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_whitespace)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "jobserver requires 1..=4096 slots and a path without whitespace",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "FIFO path contains NUL")
            })?;
            // SAFETY: name is NUL terminated and valid for the duration of mkfifo.
            if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let opened = aterm_uds::kept_out(|| {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
                    .open(path)
            });
            let mut file = match opened {
                Ok(file) => file,
                Err(error) => {
                    let _ = std::fs::remove_file(path);
                    return Err(error);
                }
            };
            if let Err(error) = file.write_all(&[b'+'; 4096][..slots as usize]) {
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            Ok(Self {
                path: path.to_owned(),
                file,
                slots,
            })
        }
        #[cfg(not(unix))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FIFO jobservers require Unix",
        ))
    }

    /// The Release grant this pool bounds.
    #[must_use]
    pub const fn slots(&self) -> u32 {
        self.slots
    }

    /// GNU make 4.4 authentication shared by Cargo, rustc and make children.
    #[must_use]
    pub fn flags(&self) -> String {
        format!("-j --jobserver-auth=fifo:{}", self.path.display())
    }

    /// Attach Cargo to this pool and charge its implicit job before it is spawned.
    /// # Errors
    /// Acquiring the implicit token fails or the caller ends the wait.
    pub fn attach_cargo(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<Token<'_>> {
        let token = self.acquire(stopped)?;
        let flags = self.flags();
        command
            .env("MAKEFLAGS", &flags)
            .env("CARGO_MAKEFLAGS", flags)
            .arg("-j")
            .arg(self.slots.to_string());
        Ok(token)
    }

    /// Reserve a client's implicit token. The caller must retain the guard until that
    /// client's process group has drained, including on failure.
    /// # Errors
    /// FIFO IO fails, or the caller ends the wait.
    pub fn acquire(&self, stopped: &dyn Fn() -> bool) -> io::Result<Token<'_>> {
        loop {
            if stopped() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "jobserver wait ended",
                ));
            }
            if let Some(token) = self.try_acquire()? {
                return Ok(token);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Take a token only if one is currently available.
    /// # Errors
    /// FIFO IO fails.
    pub fn try_acquire(&self) -> io::Result<Option<Token<'_>>> {
        let mut byte = [0];
        match (&self.file).read(&mut byte) {
            Ok(1) => Ok(Some(Token {
                pool: self,
                byte: byte[0],
            })),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
            _ => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "jobserver FIFO closed",
            )),
        }
    }
}

impl Drop for Jobserver {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One implicit job. Its lifetime prevents removing the pool while a branch uses it.
#[derive(Debug)]
pub struct Token<'a> {
    pool: &'a Jobserver,
    byte: u8,
}

impl Drop for Token<'_> {
    fn drop(&mut self) {
        let _ = (&self.pool.file).write_all(&[self.byte]);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn branches_and_their_workers_share_exactly_the_grant() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs");
        let pool = Jobserver::new(&path, 3).unwrap();
        let mut native = aterm_uds::spawnfd::command("targo");
        let mut compat = aterm_uds::spawnfd::command("cargo");
        let started = std::time::Instant::now();
        let stopped = || started.elapsed() >= aterm_spec::wait::MUST_HAPPEN;
        let first = pool.attach_cargo(&mut native, &stopped).unwrap();
        let second = pool.attach_cargo(&mut compat, &stopped).unwrap();
        for cmd in [&native, &compat] {
            for name in ["MAKEFLAGS", "CARGO_MAKEFLAGS"] {
                assert_eq!(
                    cmd.get_envs()
                        .find(|(k, _)| *k == name)
                        .and_then(|(_, v)| v),
                    Some(std::ffi::OsStr::new(&pool.flags()))
                );
            }
            assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["-j", "3"]);
        }
        // A second FIFO opener represents another process, not a private pool per branch.
        let worker = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let mut byte = [0];
        (&worker).read_exact(&mut byte).unwrap();
        assert!(
            pool.try_acquire().unwrap().is_none(),
            "three clients exhaust S=3"
        );
        drop(first);
        let returned = pool
            .try_acquire()
            .unwrap()
            .expect("implicit token returned");
        assert!(
            pool.try_acquire().unwrap().is_none(),
            "negative control: no extra implicit token"
        );
        drop((second, returned));
        (&worker).write_all(&byte).unwrap();
        let all: Vec<_> = (0..3)
            .map(|_| pool.try_acquire().unwrap().unwrap())
            .collect();
        assert!(pool.try_acquire().unwrap().is_none());
        assert!(
            Jobserver::new(&path, 3).is_err(),
            "never replace a live pool"
        );
        drop(all);
        drop(pool);
        assert!(!path.exists());
    }
}
