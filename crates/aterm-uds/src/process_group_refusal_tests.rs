// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    GroupMembers, GroupRefusal, ProcessPresence, judge_group_refusal, kill_group_until_empty,
    process_presence, settle_group_signal, short_info,
};
use crate::spawnfd::{Stdio, command};
use std::time::Duration;

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

fn eperm() -> std::io::Error {
    std::io::Error::from_raw_os_error(libc::EPERM)
}

/// `killpg(group, 0)`'s errno, which sends nothing.
fn asked(group: i32) -> Option<i32> {
    // SAFETY: two integers; signal 0 delivers nothing, and `group` > 1 names one group.
    (unsafe { libc::killpg(group, 0) } != 0)
        .then(|| std::io::Error::last_os_error().raw_os_error())
        .flatten()
}

fn listed(pids: &[i32]) -> Option<GroupMembers> {
    Some(GroupMembers {
        pids: pids.to_vec(),
        complete: true,
    })
}

/// Every member ended, a member reaped between the two reads, and a group whose listing is
/// whole and empty (what an exec leaves) settle; a running member, an unreadable one, one
/// that joined after the first listing, and a listing cut short or unread do not. NEGATIVE
/// CONTROL: let `Present` count as ended and the running member settles; drop the second
/// listing and the member that joined does.
#[test]
fn only_a_whole_stable_listing_of_ended_members_settles_a_refusal() {
    let ended = |pid| {
        if pid == 41 {
            ProcessPresence::Exited
        } else {
            ProcessPresence::Absent
        }
    };
    let judged = |first: Option<GroupMembers>, again: Option<GroupMembers>| {
        judge_group_refusal(first, ended, || again)
    };
    let both = listed(&[41, 42]);
    assert_eq!(judged(both.clone(), both.clone()), GroupRefusal::Ended);
    assert_eq!(judged(both.clone(), listed(&[41])), GroupRefusal::Ended);
    assert_eq!(judged(listed(&[]), listed(&[])), GroupRefusal::Ended);
    assert_eq!(
        judged(both.clone(), listed(&[41, 42, 43])),
        GroupRefusal::Held(43)
    );
    let cut = Some(GroupMembers {
        pids: vec![41, 42],
        complete: false,
    });
    assert_eq!(judged(both.clone(), cut.clone()), GroupRefusal::Unlisted);
    assert_eq!(judged(both.clone(), None), GroupRefusal::Unlisted);
    assert_eq!(judged(cut, both.clone()), GroupRefusal::Unlisted);
    assert_eq!(judged(None, both.clone()), GroupRefusal::Unlisted);
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        let held = judge_group_refusal(
            both.clone(),
            |pid| {
                if pid == 42 {
                    presence
                } else {
                    ProcessPresence::Exited
                }
            },
            || panic!("a held group needs no second listing"),
        );
        assert_eq!(held, GroupRefusal::Held(42), "{presence:?}");
    }
}

/// A group of one exited, unreaped leader answers `EPERM`, and is not a failure; reaped, it
/// answers `ESRCH`, which still is. NEGATIVE CONTROL: return the first signal's error from
/// `kill_group_capped` unsettled, and the zombie's group is refused.
#[test]
fn a_group_of_our_own_zombie_is_ended_not_refused() {
    let mut leader = command("/usr/bin/true")
        .setsid(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the leader");
    let pid = i32::try_from(leader.id()).expect("pid");
    aterm_spec::wait::until_within(HANG, "the leader's exit", || {
        leader.exited().expect("its exit, unreaped").then_some(())
    });
    assert_eq!(asked(pid), Some(libc::EPERM), "the macOS fact under test");
    kill_group_until_empty(pid).expect("nothing is left to signal");
    leader.wait().expect("reap the leader");
    assert_eq!(
        kill_group_until_empty(pid).unwrap_err().raw_os_error(),
        Some(libc::ESRCH),
        "reaped, the group is gone"
    );
}

/// A SIGKILLed member in its own group, parented by a process that never reaps it, is the
/// failing fixture's case made stable: the zombie is not ours, and the settling must not
/// require that it be. NEGATIVE CONTROL: accept only `ppid == getpid()` zombies and this
/// group is refused.
#[test]
fn a_strangers_zombie_group_is_ended_not_refused() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let mark = scratch.path().join("member");
    // `set -m` puts the background job in its own group; `exec` makes the parent a `sleep`,
    // which never waits, so the job's zombie stays.
    let mut parent = command("/bin/bash")
        .args([
            "-c",
            r#"set -m; /bin/sleep 600 & printf '%s' "$!" > "$0"; exec /bin/sleep 600"#,
        ])
        .arg(&mark)
        .setsid(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the parent");
    let member: i32 = aterm_spec::wait::until_within(HANG, "the member's pid", || {
        std::fs::read_to_string(&mark)
            .ok()
            .and_then(|pid| pid.parse().ok())
    });
    // Until the `exec`, bash's SIGCHLD handler would reap the member the test is about to
    // end, so the zombie is only stable once the parent's image reads as `sleep`.
    let parent_pid = i32::try_from(parent.id()).expect("pid");
    aterm_spec::wait::until_within(HANG, "the parent's exec", || {
        short_info(parent_pid, false)
            .filter(|info| info.pbsi_comm.starts_with(b"sleep"))
            .map(|_| ())
    });
    let info = short_info(member, false).expect("the member lives");
    assert_eq!(info.pbsi_pgid, member.unsigned_abs(), "its own group");
    assert_ne!(
        info.pbsi_ppid,
        std::process::id(),
        "its parent is not this test"
    );
    // SAFETY: `member` is the unreaped child of `parent`, which this test holds unreaped.
    assert_eq!(unsafe { libc::kill(member, libc::SIGKILL) }, 0);
    aterm_spec::wait::until_within(HANG, "the member's exit", || {
        (process_presence(member) == ProcessPresence::Exited).then_some(())
    });
    assert_eq!(
        asked(member),
        Some(libc::EPERM),
        "the macOS fact under test"
    );
    kill_group_until_empty(member).expect("nothing is left to signal");
    parent.kill().expect("end the parent");
    parent.wait().expect("reap the parent");
}

/// A live member this user may not signal is the `EPERM` that stays a failure, and it names
/// the member: a boot-time root daemon, asked by signal 0 only. The lowest such pid has run
/// since boot, so it does not end under the read. NEGATIVE CONTROL: settle every `EPERM`
/// and this reads `Ok`.
#[test]
fn a_live_member_this_user_may_not_signal_stays_a_failure_naming_it() {
    // SAFETY: no arguments, cannot fail.
    if unsafe { libc::getuid() } == 0 {
        return; // Root may signal anyone, so no `EPERM` exists to judge.
    }
    const PROC_ALL_PIDS: u32 = 1;
    let mut pids = vec![0i32; 8192];
    let bytes = i32::try_from(std::mem::size_of_val(pids.as_slice())).expect("bytes");
    // SAFETY: `pids` is `bytes` writable bytes of pid slots; libproc writes at most that many.
    let got =
        unsafe { crate::holders::ffi::proc_listpids(PROC_ALL_PIDS, 0, pids.as_mut_ptr(), bytes) };
    assert!(got > 0, "the process table reads");
    pids.truncate(usize::try_from(got).expect("bytes") / std::mem::size_of::<i32>());
    pids.sort_unstable();
    let daemon = pids
        .into_iter()
        .filter(|&pid| pid > 1)
        .find(|&pid| {
            short_info(pid, false).is_some_and(|info| {
                info.pbsi_ruid == 0 && info.pbsi_uid == 0 && info.pbsi_pgid == pid.unsigned_abs()
            })
        })
        .expect("a root daemon that leads its group");
    assert_eq!(asked(daemon), Some(libc::EPERM), "another user's group");
    let refused = settle_group_signal(daemon, eperm()).expect_err("it has not ended");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        refused.to_string().contains(&format!("member {daemon} ")),
        "names the member: {refused}"
    );
}

/// Any errno but `EPERM` passes through unjudged. This process's own group is the input
/// because it has a live member: judged, it would read as held and lose the errno.
/// NEGATIVE CONTROL: drop the errno guard in [`settle_group_signal`] and both errnos are lost.
#[test]
fn only_eperm_is_settled() {
    // SAFETY: no arguments, cannot fail.
    let own = unsafe { libc::getpgrp() };
    for errno in [libc::ESRCH, libc::EINVAL] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert_eq!(
            settle_group_signal(own, error).unwrap_err().raw_os_error(),
            Some(errno)
        );
    }
}
