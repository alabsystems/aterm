// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test's spawn record: [`Live`] with every run it makes written down first, as that run
//! was asked for, so a test can say what was (and was not) spawned on the real host.

use super::{Cmd, Live, Ran, RunFailed, Scratch, Stat, System, User};
use std::io;
use std::path::Path;
use std::sync::Mutex;

/// Every run made through a real `System`, as that `System` spawns it: the [`Cmd`] (its
/// program, arguments, environment and directory; [`Live::spawned`] gives the argv and the
/// whole environment), plus the bytes it was handed on standard input.
pub(crate) struct Spawns<'a> {
    live: &'a Live,
    record: Mutex<Vec<(Cmd, Option<Vec<u8>>)>>,
}

impl<'a> Spawns<'a> {
    /// A record over `live`, empty.
    pub(crate) fn new(live: &'a Live) -> Self {
        Self {
            live,
            record: Mutex::new(Vec::new()),
        }
    }

    /// Every run so far, in order.
    pub(crate) fn record(&self) -> Vec<(Cmd, Option<Vec<u8>>)> {
        self.record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn note(&self, cmd: &Cmd, input: Option<&[u8]>) {
        self.record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((cmd.clone(), input.map(<[u8]>::to_vec)));
    }
}

impl System for Spawns<'_> {
    fn scratch_dir(&self, parent: Option<&Path>, label: &str) -> io::Result<Scratch<'_>> {
        self.live.scratch_dir(parent, label)
    }
    fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        self.note(cmd, None);
        self.live.run(cmd)
    }
    fn run_complete_stdout(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        self.note(cmd, None);
        self.live.run_complete_stdout(cmd)
    }
    fn run_complete_stdout_input(&self, cmd: &Cmd, input: &[u8]) -> Result<Ran, RunFailed> {
        self.note(cmd, Some(input));
        self.live.run_complete_stdout_input(cmd, input)
    }
    fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed> {
        self.note(cmd, None);
        self.live.run_tty(cmd)
    }
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.live.read_file(path)
    }
    fn read_owned_file(
        &self,
        path: &Path,
        uid: u32,
        max: usize,
        private: bool,
    ) -> io::Result<Vec<u8>> {
        self.live.read_owned_file(path, uid, max, private)
    }
    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        self.live.write_file(path, bytes, mode)
    }
    fn write_user_file(&self, user: &User, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        self.live.write_user_file(user, path, bytes, mode)
    }
    fn durable_dir(&self, path: &Path) -> io::Result<()> {
        self.live.durable_dir(path)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.live.remove(path)
    }
    fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.live.create_dir_all(path, mode)
    }
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()> {
        self.live.chown(path, uid, gid)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        self.live.stat(path)
    }
    fn list_dir(&self, path: &Path) -> io::Result<Vec<String>> {
        self.live.list_dir(path)
    }
    fn sha256(&self, path: &Path) -> io::Result<String> {
        self.live.sha256(path)
    }
    fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()> {
        self.live.copy_file(from, to, mode)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.live.rename(from, to)
    }
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.live.rename_noreplace(from, to)
    }
    fn euid(&self) -> u32 {
        self.live.euid()
    }
    fn now(&self) -> u64 {
        self.live.now()
    }
}
