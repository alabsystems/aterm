// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual inherited close-only descriptions, without orphaned test processes.
//! The supervisor creates and consumes the original NamedLock. Two direct
//! children inherit its description, then the supervisor drops every copy.
//! Creator PID is not lock authority: either child keeps the original lock
//! after the other exits, and only their final close permits a fresh open.

#![cfg(unix)]

use aterm_spec::wait::{MUST_HAPPEN, until_within, within};
use aterm_toolchain::{
    named_lock::{self, Holder, NamedLock, Wait},
    record_lock::{CloseOnlyLock, RecordLock},
};
use aterm_uds::{process, spawnfd};
use std::{
    fs::{self, File, Metadata, OpenOptions, TryLockError},
    os::unix::{fs::MetadataExt as _, process::ExitStatusExt as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const ROLE: &str = "ATERM_CLOSE_ONLY_LOCK_ROLE";
const WORLD: &str = "ATERM_CLOSE_ONLY_LOCK_WORLD";
const GUARD: &str = "ATERM_CLOSE_ONLY_LOCK_FD";
const DEV: &str = "ATERM_CLOSE_ONLY_LOCK_DEV";
const INO: &str = "ATERM_CLOSE_ONLY_LOCK_INO";
const ACTOR: &str = "hold_inherited_description";

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn publish(path: &Path, bytes: &[u8]) {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".pending");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes).unwrap();
    fs::rename(temporary, path).unwrap();
}

#[test]
fn hold_inherited_description() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    assert!(matches!(role.as_str(), "owner" | "keeper"));
    let root = PathBuf::from(std::env::var_os(WORLD).unwrap());
    let fd: i32 = std::env::var(GUARD).unwrap().parse().unwrap();
    let expected = (
        std::env::var(DEV).unwrap().parse::<u64>().unwrap(),
        std::env::var(INO).unwrap().parse::<u64>().unwrap(),
    );
    let metadata = spawnfd::descriptor_metadata(fd).unwrap();
    assert!(metadata.is_file());
    assert_eq!(identity(&metadata), expected);
    // Inspect the original slot without adopting it into a guard or File that
    // could explicitly unlock it. The kernel closes it at this actor's exit.
    assert_blocked(&root.join("lane.lock"));
    publish(
        &root.join(format!("{role}.ready")),
        format!("{} {} {}\n", std::process::id(), expected.0, expected.1).as_bytes(),
    );
    let end = Instant::now() + MUST_HAPPEN;
    let mut answered = None;
    while !root.join(format!("{role}.release")).exists() {
        assert!(
            Instant::now() < end,
            "supervisor did not release actual FD holder"
        );
        if let Ok(challenge) = fs::read(root.join(format!("{role}.challenge")))
            && answered.as_ref() != Some(&challenge)
        {
            assert_eq!(
                identity(&spawnfd::descriptor_metadata(fd).unwrap()),
                expected
            );
            let mut response =
                format!("{} {} {}\n", std::process::id(), expected.0, expected.1).into_bytes();
            response.extend_from_slice(&challenge);
            publish(&root.join(format!("{role}.response")), &response);
            answered = Some(challenge);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_blocked(path: &Path) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert!(
        matches!(
            RecordLock::try_lock(file, named_lock::RECORD),
            Err(TryLockError::WouldBlock)
        ),
        "an independent open unexpectedly acquired {}",
        path.display()
    );
}

struct Actor {
    child: spawnfd::Child,
    root: PathBuf,
    role: &'static str,
}

impl Actor {
    fn start(root: &Path, role: &'static str, held: &CloseOnlyLock, original: (u64, u64)) -> Self {
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", ACTOR, "--test-threads=1", "--nocapture"])
            .env(ROLE, role)
            .env(WORLD, root)
            .env(DEV, original.0.to_string())
            .env(INO, original.1.to_string())
            .stdin(spawnfd::Stdio::null())
            .stdout(File::create(root.join(format!("{role}.stdout"))).unwrap())
            .stderr(File::create(root.join(format!("{role}.stderr"))).unwrap());
        command
            .inherit_guards(vec![held.try_clone_fd().unwrap()])
            .unwrap();
        let fd = command.guard_targets().next().unwrap();
        assert_eq!(command.guard_targets().len(), 1);
        command.env(GUARD, fd.to_string());
        let child = command.spawn().unwrap();
        // Command owns the parent-side duplicate after spawn; drop it here.
        drop(command);
        let mut actor = Self {
            child,
            root: root.to_owned(),
            role,
        };
        until_within(
            MUST_HAPPEN,
            "actual child to verify its inherited lock",
            || {
                assert!(
                    actor.child.try_wait().unwrap().is_none(),
                    "{role} exited before its descriptor receipt"
                );
                fs::read_to_string(root.join(format!("{role}.ready"))).ok()
            },
        );
        assert_eq!(
            fs::read_to_string(root.join(format!("{role}.ready"))).unwrap(),
            format!("{} {} {}\n", actor.child.id(), original.0, original.1)
        );
        actor
    }

    fn challenge(&mut self, original: (u64, u64)) {
        let birth = process::read_identity(i32::try_from(self.child.id()).unwrap())
            .expect("actual owned holder's native birth");
        let nonce = aterm_uds::rand::hex_token::<16>().unwrap();
        publish(
            &self.root.join(format!("{}.challenge", self.role)),
            nonce.as_bytes(),
        );
        let response = until_within(
            MUST_HAPPEN,
            "same inherited FD holder's fresh response",
            || {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "FD holder exited during challenge"
                );
                fs::read(self.root.join(format!("{}.response", self.role))).ok()
            },
        );
        assert_eq!(
            response,
            format!("{} {} {}\n{nonce}", self.child.id(), original.0, original.1).as_bytes()
        );
        assert_eq!(
            process::read_identity(i32::try_from(self.child.id()).unwrap()),
            Some(birth)
        );
    }

    fn release(&mut self) -> std::process::ExitStatus {
        publish(
            &self.root.join(format!("{}.release", self.role)),
            b"owned supervisor release\n",
        );
        until_within(MUST_HAPPEN, "actual owned FD holder to exit", || {
            self.child.try_wait().unwrap()
        })
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = fs::write(
            self.root.join(format!("{}.release", self.role)),
            b"cleanup\n",
        );
        if within(Duration::from_secs(2), Duration::from_millis(10), || {
            self.child.try_wait().ok().flatten()
        })
        .is_none()
        {
            // This is the actual Child receipt created above, never an arbitrary PID.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct World(Option<aterm_tempfile::TempDir>);
impl World {
    fn new() -> Self {
        Self(Some(aterm_tempfile::tempdir().unwrap()))
    }
    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}
impl Drop for World {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "close-only lock: kept {}",
                self.0.take().unwrap().keep().display()
            );
        }
    }
}

#[test]
fn actual_child_holders_keep_the_consumed_lock_after_normal_and_sigkill_fd_loss() {
    for abrupt in [false, true] {
        let world = World::new();
        let path = world.path().join("lane.lock");
        let named: NamedLock = named_lock::take(
            &path,
            &Holder::me("close-only subprocess control", world.path()),
            Wait::No,
            &mut |_, _| panic!("fresh lock unexpectedly held"),
            &|| false,
        )
        .unwrap();
        let original = identity(&named.metadata().unwrap());
        let record = fs::read(&path).unwrap();
        assert!(!record.is_empty());
        let held = named.into_close_only();
        assert_eq!(
            fs::read(&path).unwrap(),
            record,
            "consumption must not clear the holder record"
        );
        let mut owner = Actor::start(world.path(), "owner", &held, original);
        let mut keeper = Actor::start(world.path(), "keeper", &held, original);
        drop(held);
        assert_eq!(
            fs::read(&path).unwrap(),
            record,
            "parent close must not truncate the advisory record"
        );
        assert_blocked(&path);
        let status = if abrupt {
            owner.child.kill().unwrap();
            let status = until_within(MUST_HAPPEN, "actual owner SIGKILL/reap", || {
                owner.child.try_wait().unwrap()
            });
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            status
        } else {
            owner.release()
        };
        assert_eq!(status.success(), !abrupt);
        assert_blocked(&path);
        // Mint the challenge only after actual owned-owner exit and reap.
        keeper.challenge(original);
        assert_blocked(&path);
        assert_eq!(
            fs::read(&path).unwrap(),
            record,
            "inherited close must not clear the record"
        );
        assert!(keeper.release().success());
        let reacquired = named_lock::take(
            &path,
            &Holder::me("after final actual close", world.path()),
            Wait::No,
            &mut |_, _| panic!("final close did not release original lock"),
            &|| false,
        )
        .unwrap();
        assert_eq!(
            identity(&reacquired.metadata().unwrap()),
            original,
            "fresh open must reacquire the same physical inode"
        );
        eprintln!(
            "close-only: owner={} exit={status} keeper={} original_inode={original:?}; post-owner challenge/blocked opens and final-close reacquisition passed",
            owner.child.id(),
            keeper.child.id()
        );
    }
}
