// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual private-prelude/target FD lifetime after original sender death. This
//! is a kernel custody control, not an SDK, graph issuer or CPU-retirement claim.
use super::*;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::{fs::MetadataExt as _, process::ExitStatusExt as _},
    },
    time::{Duration, Instant},
};
const CASE: &str = "actor::owner::bootstrap_object::inheritance_tests::native_object_output_ofds_survive_sender_death";
const PARENT: &str =
    "actor::owner::bootstrap_object::inheritance_tests::native_object_original_parent_actor";
const TARGET: &str =
    "actor::owner::bootstrap_object::inheritance_tests::native_object_guard_target_actor";
const PRELUDE: &str =
    "actor::owner::bootstrap_object::inheritance_tests::native_object_guard_prelude_actor";
struct HeldLock<'a>(&'a File);
impl<'a> HeldLock<'a> {
    fn take(file: &'a File) -> Result<Self, std::fs::TryLockError> {
        #[expect(
            clippy::disallowed_methods,
            reason = "this fixture guard explicitly unlocks in Drop; sender is killed only after target GO"
        )]
        file.try_lock()?;
        Ok(Self(file))
    }
}
impl Drop for HeldLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
struct Child(spawnfd::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn record(path: &std::path::Path, bytes: impl AsRef<[u8]>) {
    let pending = path.with_extension("writing");
    fs::write(&pending, bytes).unwrap();
    fs::rename(pending, path).unwrap();
}
fn await_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "native actor never produced {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn native_object_guard_prelude_actor() {
    let Some(fd) = std::env::var_os("ATERM_TEST_OBJECT_PRELUDE") else {
        return;
    };
    let result = crate::metadata_prelude::entry(&[
        OsString::from("__resource-metadata-exec-v1"),
        OsString::from(format!("--control-fd={}", fd.to_str().unwrap())),
    ])
    .unwrap();
    std::process::exit(if result == std::process::ExitCode::SUCCESS {
        0
    } else {
        75
    });
}
#[test]
fn native_object_guard_target_actor() {
    let Some(root) = std::env::var_os("ATERM_TEST_OBJECT_ROOT") else {
        return;
    };
    assert!(crate::metadata_prelude::no_fork(std::process::id()).unwrap());
    assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
    assert_eq!(unsafe { libc::getpgid(0) }, std::process::id() as i32);
    let expected = std::env::var("ATERM_TEST_OBJECT_IDENTITIES").unwrap();
    let actual = (3..=9)
        .map(|fd| {
            let m = spawnfd::descriptor_metadata(fd).unwrap();
            format!("{}:{}", m.dev(), m.ino())
        })
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        actual, expected,
        "all seven actual descriptions crossed both execs"
    );
    let path = std::path::Path::new(&root);
    let birth = aterm_uds::process::read_birth(std::process::id() as i32)
        .unwrap()
        .0;
    record(
        &path.join("target-ready"),
        format!("{} {birth}", std::process::id()),
    );
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    assert_eq!(byte, [0x61]);
    record(
        &path.join("target-finished"),
        b"actual source/output originals still held",
    );
}
#[test]
fn native_object_original_parent_actor() {
    let Some(path) = std::env::var_os("ATERM_TEST_OBJECT_ROOT") else {
        return;
    };
    let path = std::path::Path::new(&path);
    crate::runtime::test_endpoint().unwrap();
    let output = File::open(path).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("output.lock"))
        .unwrap();
    let _held = HeldLock::take(&lock).unwrap();
    let source_root = File::open(path).unwrap();
    let source = File::open(path.join("input.rs")).unwrap();
    let sdk_input = File::open(path.join("selected-input")).unwrap();
    let (read, write) = aterm_uds::pipe().unwrap();
    let originals: Vec<OwnedFd> = vec![
        read.into(),
        write.into(),
        sdk_input.into(),
        source_root.into(),
        source.into(),
        output.into(),
        lock.try_clone().unwrap().into(),
    ];
    let identities = originals
        .iter()
        .map(|fd| {
            let m = spawnfd::descriptor_metadata(fd.as_raw_fd()).unwrap();
            format!("{}:{}", m.dev(), m.ino())
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target
        .args(["--exact", TARGET, "--nocapture"])
        .env_clear()
        .env("ATERM_TEST_OBJECT_ROOT", path)
        .env("ATERM_TEST_OBJECT_IDENTITIES", &identities)
        .current_dir(path);
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut owner, _) =
        crate::metadata_prelude::Owner::new(&target, &[3, 4, 5, 6, 7, 8, 9], deadline).unwrap();
    let extra = owner.extra().unwrap();
    let control = extra.1;
    let mut prelude = spawnfd::command(std::env::current_exe().unwrap());
    prelude
        .args(["--exact", PRELUDE, "--nocapture"])
        .env_clear()
        .env("ATERM_TEST_OBJECT_ROOT", path)
        .env("ATERM_TEST_OBJECT_IDENTITIES", identities)
        .env("ATERM_TEST_OBJECT_PRELUDE", control.to_string())
        .current_dir(path)
        .setsid(true)
        .stdin(spawnfd::Stdio::inherit())
        .start_suspended(true);
    let mut guards: Vec<_> = originals.into_iter().zip(3..=9).collect();
    guards.push(extra);
    prelude.inherit_guards_at(guards).unwrap();
    owner.configured(&mut prelude).unwrap();
    let mut child = Child(prelude.spawn().unwrap());
    drop(prelude);
    while !owner
        .poll(&mut child.0, |pid| {
            assert!(crate::metadata_prelude::no_fork(pid)?);
            Ok(())
        })
        .unwrap()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    child.0.resume().unwrap();
    await_file(&path.join("target-ready"));
    record(
        &path.join("parent-ready"),
        b"actual target GO and seven original descriptions",
    );
    // Test controller kills this actual sender, so no Drop releases its Child.
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
#[test]
fn native_object_output_ofds_survive_sender_death() {
    if !aterm_uds::test_binary::isolated_lifecycle(CASE) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = fs::canonicalize(scratch.path()).unwrap();
    fs::write(path.join("output.lock"), b"original output OFD").unwrap();
    fs::write(path.join("input.rs"), wire::SOURCE_BYTES).unwrap();
    fs::write(
        path.join("selected-input"),
        b"readonly fixture, no SDK authority",
    )
    .unwrap();
    let (lock_read, mut release) = aterm_uds::pipe().unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", PARENT, "--nocapture"])
        .env("ATERM_TEST_OBJECT_ROOT", &path)
        .stdin(spawnfd::Stdio::from(lock_read));
    let mut parent = Child(command.spawn().unwrap());
    drop(command);
    let ready_deadline = Instant::now() + Duration::from_secs(60);
    while !path.join("parent-ready").is_file() {
        assert!(
            parent.0.try_wait().unwrap().is_none(),
            "original sender exited before target GO"
        );
        assert!(
            Instant::now() < ready_deadline,
            "original sender never reached target GO"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let actual = fs::read_to_string(path.join("target-ready")).unwrap();
    let mut words = actual.split_whitespace();
    let pid: i32 = words.next().unwrap().parse().unwrap();
    let birth: u64 = words.next().unwrap().parse().unwrap();
    assert!(words.next().is_none());
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("output.lock"))
        .unwrap();
    assert!(matches!(
        HeldLock::take(&lock),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    parent.0.kill().unwrap();
    assert_eq!(parent.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert_eq!(aterm_uds::process::read_birth(pid).unwrap().0, birth);
    assert!(
        matches!(
            HeldLock::take(&lock),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "sender death must not release target's original output OFD"
    );
    release.write_all(&[0x61]).unwrap();
    drop(release);
    await_file(&path.join("target-finished"));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match HeldLock::take(&lock) {
            Ok(_held) => break,
            Err(std::fs::TryLockError::WouldBlock) => (),
            Err(error) => panic!("actual original output lock failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "actual target did not close original output OFD"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // Actual target-FD disposal only; no PID absence/wait/exit is converted to
    // Host closure, GNU return, source release or a completed SDK operation.
}
