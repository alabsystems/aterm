// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;

fn stand_in(cloexec: bool) -> OwnedFd {
    let null = std::fs::File::open("/dev/null").expect("open /dev/null");
    let cmd = if cloexec {
        libc::F_DUPFD_CLOEXEC
    } else {
        libc::F_DUPFD
    };
    // SAFETY: F_DUPFD(_CLOEXEC) on a descriptor this test owns.
    let fd = unsafe { libc::fcntl(null.as_raw_fd(), cmd, 64) };
    assert!(fd >= 64, "a stand-in at 64 or above: {fd}");
    // SAFETY: a fresh descriptor this test exclusively owns.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn child_holds(numbers: &[i32], hook: Option<Vec<i32>>) -> String {
    let probe = numbers
        .iter()
        .map(|&fd| {
            let held = aterm_spec::host::sh_held(fd);
            format!("{held} && got=\"$got held:{fd}\"; ")
        })
        .collect::<String>();
    let Some(handed) = hook else {
        let dir = aterm_tempfile::tempdir().expect("a temp dir");
        let report = dir.path().join("holds");
        let status = aterm_uds::spawnfd::transparent("/bin/sh")
            .args([
                "-c",
                &format!("got=; {probe}exec >\"$1\" </dev/null; printf '%s end\\n' \"$got\""),
                "sh",
            ])
            .arg(&report)
            .status()
            .expect("run the control");
        assert!(status.success(), "{status}");
        return std::fs::read_to_string(&report)
            .expect("the control's report")
            .trim()
            .to_string();
    };
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", &format!("got=; {probe}printf '%s end\\n' \"$got\"")]);
    super::hand_to_fork_lane_child(&mut cmd, handed);
    let out = cmd.output().expect("run the stand-in successor");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_fork_lane_successor_holds_what_it_is_handed_and_nothing_else() {
    let stray = stand_in(false);
    let handed = stand_in(true);
    let (s, h) = (stray.as_raw_fd(), handed.as_raw_fd());
    assert_eq!(
        child_holds(&[s, h], None),
        format!("held:{s} end"),
        "CONTROL: the transparent spawn carries the stray and not the flagged one"
    );
    assert_eq!(
        child_holds(&[s, h], Some(vec![h])),
        format!("held:{h} end"),
        "the successor holds the handed descriptor and not the stray"
    );
    drop((stray, handed));
}

/// `FD_CLOFORK`, which Apple's libc crate does not name.
#[cfg(target_vendor = "apple")]
const FD_CLOFORK: i32 = 2;

fn descriptor_flags(fd: i32) -> i32 {
    // SAFETY: F_GETFD reads one descriptor's flags and touches no memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "read an owned descriptor's flags");
    flags
}

#[cfg(target_vendor = "apple")]
fn kernel_retains_close_on_fork() -> bool {
    use std::os::fd::AsFd as _;
    // Use the managed constructor, then clear only this unrelated probe's mark so the
    // capability oracle cannot inherit the constructor's answer. The attempt ends stay untouched.
    let (probe, _peer) = aterm_uds::pipe().expect("an independent capability pipe");
    let fd = probe.as_raw_fd();
    let flags = descriptor_flags(fd);
    // SAFETY: F_SETFD changes only the test's owned probe descriptor, before any child spawn.
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !FD_CLOFORK) },
        0,
        "clear the unrelated probe's fork mark"
    );
    assert_eq!(descriptor_flags(fd) & FD_CLOFORK, 0);
    astream_sys::close_on_fork(probe.as_fd()).expect("the kernel answers the flag request")
}

/// The fork lane's successor proves its adoption on the attempt's proof pipe and takes the
/// Commit from its Commit pipe. Both ends request close-on-fork (`AttemptPipes::make`), but
/// some Apple kernels do not retain that flag. The spawn must hand both ends to the fork for
/// the successor to start, and restore the parent's original flags on every kernel.
#[test]
fn the_fork_lane_successor_transfers_the_proof_and_commit_ends_and_restores_their_flags() {
    use std::io::{Read as _, Write as _};
    let super::AttemptPipes {
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = super::AttemptPipes::make().expect("the attempt's proof and Commit pipes");
    let handed = vec![proof_wr.as_raw_fd(), commit_rd.as_raw_fd()];
    let original_flags: Vec<_> = handed.iter().map(|&fd| descriptor_flags(fd)).collect();
    assert!(
        original_flags
            .iter()
            .all(|flags| flags & libc::FD_CLOEXEC != 0)
    );
    #[cfg(target_vendor = "apple")]
    {
        let retained = kernel_retains_close_on_fork();
        assert!(
            original_flags
                .iter()
                .all(|flags| (flags & FD_CLOFORK != 0) == retained),
            "the pipe ends retain close-on-fork exactly when this kernel supports it"
        );
    }

    // The stand-in opens each handed end by its `/dev/fd` path: a POSIX sh such as dash takes
    // only one digit in `<&N` and refuses a pipe end numbered 10 or above ("Bad fd number").
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args([
        "-c",
        "read -r word < \"/dev/fd/$2\" && printf 'proved %s\\n' \"$word\" > \"/dev/fd/$1\"",
        "sh",
        &handed[0].to_string(),
        &handed[1].to_string(),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null());
    super::hand_to_fork_lane_child(&mut cmd, handed.clone());
    let mut child = super::spawn_fork_lane_successor(&mut cmd, &handed)
        .expect("the fork lane's successor starts holding the proof and Commit ends");
    assert_eq!(
        handed
            .iter()
            .map(|&fd| descriptor_flags(fd))
            .collect::<Vec<_>>(),
        original_flags,
        "each handed end's original flags are restored once the spawn returned"
    );
    drop((proof_wr, commit_rd));
    let mut commit = std::fs::File::from(commit_wr);
    commit
        .write_all(b"commit\n")
        .expect("send the stand-in its Commit");
    drop(commit);
    let (sent, proof) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut proved = String::new();
        let read = std::fs::File::from(proof_rd)
            .read_to_string(&mut proved)
            .map(|_| proved);
        let _ = sent.send(read);
    });
    let Ok(proved) = proof.recv_timeout(aterm_spec::wait::MUST_HAPPEN) else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the stand-in successor never closed its proof end");
    };
    let status = child.wait().expect("reap the stand-in successor");
    assert!(status.success(), "{status}");
    assert_eq!(
        proved.expect("read the stand-in's proof"),
        "proved commit\n",
        "the successor wrote its proof on the handed proof end after reading the handed Commit end"
    );
}

#[test]
fn the_cold_lane_exec_hands_over_nothing_this_process_holds() {
    const STAGE: &str = "ATERM_TEST_COLD_EXEC_HAND_OVER";
    if let Ok(stage) = std::env::var(STAGE) {
        let stray = stand_in(false);
        let fd = stray.as_raw_fd();
        let _lifeline = (stage == "hand-over").then(super::cold_exec_hand_over);
        let error = std::process::Command::new("/bin/sh")
            .args(["-c", &aterm_spec::host::held_probe(fd)])
            .exec();
        panic!("the probe did not replace this helper: {error}");
    }
    let probe = |stage: &str| {
        let mut helper = crate::test_alone::StandIn::this_test();
        helper.command().env(STAGE, stage);
        helper
            .spawn()
            .selected()
            .status
            .and_then(|status| status.code())
    };
    assert_eq!(
        probe("plain"),
        Some(3),
        "CONTROL: a plain exec hands the probe the stray"
    );
    assert_eq!(
        probe("hand-over"),
        Some(0),
        "the cold lane's exec handed the probe the stray"
    );
}
