// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{
    fs::File,
    os::{fd::AsFd as _, unix::fs::MetadataExt as _},
    time::Duration,
};

const STAGE: &str = "ATERM_MAPPED_GUARD_STAGE";
const CASE: &str =
    "spawnfd::mapped_tests::mapped_guards_preserve_parent_table_and_shared_description";
fn id(fd: RawFd) -> Option<(u64, u64)> {
    descriptor_metadata(fd).ok().map(|m| (m.dev(), m.ino()))
}

#[test]
fn mapped_guards_preserve_parent_table_and_shared_description() {
    if std::env::var(STAGE).as_deref() == Ok("observe") {
        let a = std::fs::metadata(std::env::var_os("ATERM_MAPPED_A").unwrap()).unwrap();
        let b = std::fs::metadata(std::env::var_os("ATERM_MAPPED_B").unwrap()).unwrap();
        let targets: Vec<RawFd> = std::env::var("ATERM_MAPPED_TARGETS")
            .unwrap()
            .split(',')
            .map(|n| n.parse().unwrap())
            .collect();
        let wide = targets.len() == MAX_GUARD_FDS;
        for (index, &target) in targets.iter().enumerate() {
            let expected = if wide || index % 2 == 0 { &a } else { &b };
            assert_eq!(id(target), Some((expected.dev(), expected.ino())));
        }
        let original =
            std::fs::metadata(std::env::var_os("ATERM_MAPPED_UNREQUESTED").unwrap()).unwrap();
        let fd: RawFd = std::env::var("ATERM_MAPPED_UNREQUESTED_FD")
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(id(fd), Some((original.dev(), original.ino())));
        let lock_fd = if wide { targets[0] } else { targets[2] };
        // SAFETY: this explicit child target has no Rust owner in the fresh
        // observer image; this File alone owns its inherited description.
        let shared = unsafe { File::from_raw_fd(lock_fd) };
        #[expect(
            clippy::disallowed_methods,
            reason = "same inherited open description must retain its exclusive fixture lock"
        )]
        shared.try_lock().unwrap();
        let probe = File::open(std::env::var_os("ATERM_MAPPED_A").unwrap()).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "independently opened description must not acquire the inherited fixture lock"
        )]
        let failed = probe.try_lock();
        assert!(matches!(failed, Err(std::fs::TryLockError::WouldBlock)));
        println!("mapped-guard-lock-and-targets-proved");
        return;
    }
    if std::env::var(STAGE).as_deref() != Ok("actor") {
        let mut actor = command(std::env::current_exe().unwrap());
        actor
            .args(["--exact", CASE, "--nocapture"])
            .env(STAGE, "actor");
        let out = crate::deadline::output_within(&mut actor, Duration::from_secs(60)).unwrap();
        assert!(out.status.success(), "{out:?}");
        return;
    }
    // Raise only this isolated fixture actor's soft limit; preserve its hard
    // limit and leave the actual test runner's descriptor placement unchanged.
    let mut limit = Rlimit { cur: 0, max: 0 };
    // SAFETY: these native resource calls use live exact-layout storage and
    // raise only this isolated process's soft limit within its current hard cap.
    unsafe {
        assert_eq!(getrlimit(RLIMIT_NOFILE, &mut limit), 0);
        let raised = Rlimit {
            cur: limit.cur.max(4096.min(limit.max)),
            max: limit.max,
        };
        assert_eq!(setrlimit(RLIMIT_NOFILE, &raised), 0);
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let pa = scratch.path().join("a");
    let pb = scratch.path().join("b");
    let pu = scratch.path().join("unrequested");
    let a = File::create(&pa).unwrap();
    let b = File::create(&pb).unwrap();
    let u = File::create(&pu).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "fixture exclusive open-description lock remains held through child proof"
    )]
    a.try_lock().unwrap();
    // SAFETY: mark only this owned nonsensitive fixture inheritable, to verify
    // CLOEXEC_DEFAULT still excludes unrequested parent descriptions.
    assert_eq!(unsafe { fcntl(u.as_raw_fd(), F_SETFD, 0) }, 0);
    let original_a = id(a.as_raw_fd());
    let original_b = id(b.as_raw_fd());
    for targets in [
        vec![b.as_raw_fd(), a.as_raw_fd(), 900, 901],
        (900..900 + MAX_GUARD_FDS as i32).collect(),
    ] {
        let before: Vec<_> = targets.iter().map(|&fd| id(fd)).collect();
        let mut child = command(std::env::current_exe().unwrap());
        let mut guards: Vec<_> = targets
            .iter()
            .enumerate()
            .map(|(index, &at)| {
                let source = if targets.len() == MAX_GUARD_FDS || index % 2 == 0 {
                    &a
                } else {
                    &b
                };
                (source.as_fd().try_clone_to_owned().unwrap(), at)
            })
            .collect();
        let rest = guards.split_off(targets.len() / 2);
        child.inherit_guards_at(guards).unwrap();
        let original_targets = child.guard_targets().collect::<Vec<_>>();
        assert_eq!(
            child
                .append_guards_at(vec![(a.as_fd().try_clone_to_owned().unwrap(), targets[0])])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(child.guard_targets().collect::<Vec<_>>(), original_targets);
        child.append_guards_at(rest).unwrap();
        assert_eq!(child.guard_targets().collect::<Vec<_>>(), targets);
        if targets.len() == MAX_GUARD_FDS {
            assert_eq!(
                child
                    .append_guards_at(vec![(a.as_fd().try_clone_to_owned().unwrap(), 2000)])
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(child.guard_targets().collect::<Vec<_>>(), targets);
        }
        assert_eq!(targets.iter().map(|&fd| id(fd)).collect::<Vec<_>>(), before);
        let mut exec = command("/definitely-absent/mapped-guard-fixture");
        exec.inherit_guards_at(vec![(a.as_fd().try_clone_to_owned().unwrap(), 900)])
            .unwrap();
        assert_eq!(exec.exec().kind(), io::ErrorKind::Unsupported);
        child
            .args(["--exact", CASE, "--nocapture"])
            .env(STAGE, "observe")
            .env("ATERM_MAPPED_A", &pa)
            .env("ATERM_MAPPED_B", &pb)
            .env("ATERM_MAPPED_UNREQUESTED", &pu)
            .env("ATERM_MAPPED_UNREQUESTED_FD", u.as_raw_fd().to_string())
            .env(
                "ATERM_MAPPED_TARGETS",
                targets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        let out = crate::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("mapped-guard-lock-and-targets-proved")
        );
        assert_eq!(id(a.as_raw_fd()), original_a);
        assert_eq!(id(b.as_raw_fd()), original_b);
        assert_eq!(targets.iter().map(|&fd| id(fd)).collect::<Vec<_>>(), before);
    }
    // Consume source slots which are each other's requested child targets.
    // The second copy must not overwrite the first source before placement.
    let ca = a.as_fd().try_clone_to_owned().unwrap();
    let cb = b.as_fd().try_clone_to_owned().unwrap();
    let targets = [cb.as_raw_fd(), ca.as_raw_fd(), 900, 901];
    let mut crossed = command(std::env::current_exe().unwrap());
    crossed
        .inherit_guards_at(vec![
            (ca, targets[0]),
            (cb, targets[1]),
            (a.as_fd().try_clone_to_owned().unwrap(), 900),
            (b.as_fd().try_clone_to_owned().unwrap(), 901),
        ])
        .unwrap();
    assert_eq!(id(targets[0]), None);
    assert_eq!(id(targets[1]), None);
    crossed
        .args(["--exact", CASE, "--nocapture"])
        .env(STAGE, "observe")
        .env("ATERM_MAPPED_A", &pa)
        .env("ATERM_MAPPED_B", &pb)
        .env("ATERM_MAPPED_UNREQUESTED", &pu)
        .env("ATERM_MAPPED_UNREQUESTED_FD", u.as_raw_fd().to_string())
        .env(
            "ATERM_MAPPED_TARGETS",
            targets
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
    let out = crate::deadline::output_within(&mut crossed, Duration::from_secs(60)).unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("mapped-guard-lock-and-targets-proved"));
    assert_eq!(id(a.as_raw_fd()), original_a);
    assert_eq!(id(b.as_raw_fd()), original_b);
    let mut bad = command("/usr/bin/true");
    for targets in [
        vec![2],
        vec![900, 900],
        vec![i32::MAX],
        (1000..1131).collect(),
    ] {
        let inputs = targets
            .into_iter()
            .map(|at| (a.as_fd().try_clone_to_owned().unwrap(), at))
            .collect();
        assert_eq!(
            bad.inherit_guards_at(inputs).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(bad.guard_targets().len(), 0);
    }
}
