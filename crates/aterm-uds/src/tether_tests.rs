// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Through, lost_of, parent_pid};
use std::ffi::OsStr;

#[test]
#[cfg(unix)]
fn an_anchor_requires_a_terminal_sigchld_for_the_actual_child() {
    for code in [libc::CLD_EXITED, libc::CLD_KILLED, libc::CLD_DUMPED] {
        assert!(super::terminal_anchor_wait(42, libc::SIGCHLD, 42, code));
    }
    for code in [libc::CLD_STOPPED, libc::CLD_TRAPPED, libc::CLD_CONTINUED] {
        assert!(!super::terminal_anchor_wait(42, libc::SIGCHLD, 42, code));
    }
    assert!(!super::terminal_anchor_wait(
        42,
        libc::SIGCHLD,
        43,
        libc::CLD_EXITED
    ));
    assert!(!super::terminal_anchor_wait(42, 0, 42, libc::CLD_EXITED));
}

#[test]
fn parent_pid_accepts_only_unsigned_pids_above_init() {
    assert_eq!(parent_pid(OsStr::new("42")), Some(42));
    assert_eq!(parent_pid(OsStr::new("1")), None);
    assert_eq!(parent_pid(OsStr::new("0")), None);
    assert_eq!(parent_pid(OsStr::new("+42")), None);
    assert_eq!(parent_pid(OsStr::new("-42")), None);
}

#[test]
fn a_file_put_in_place_of_the_recorded_executable_is_not_run_through() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let exe = dir.path().join("aterm");
    std::fs::write(&exe, b"the build this process started from").expect("write");
    let through = Through::of(exe.clone());
    assert_eq!(
        through.unchanged(),
        Some(exe.as_path()),
        "the recorded file"
    );
    let next = dir.path().join("aterm.next");
    std::fs::write(&next, b"another build").expect("write");
    std::fs::rename(&next, &exe).expect("rename over");
    assert_eq!(through.unchanged(), None, "another file at its path");
    std::fs::remove_file(&exe).expect("remove");
    assert_eq!(through.unchanged(), None, "no file at its path");
    assert_eq!(
        Through::of(exe.clone()).unchanged(),
        None,
        "nothing recorded"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_own_tether_record_requires_the_loaded_image() {
    let exe = std::env::current_exe().expect("running test binary");
    assert_eq!(Through::own(exe.clone()).unchanged(), Some(exe.as_path()));

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, b"a different file at the launch path").unwrap();
    assert_eq!(
        Through::of(replacement.clone()).unchanged(),
        Some(replacement.as_path()),
        "an explicit test host may be another image"
    );
    assert_eq!(
        Through::own(replacement).unchanged(),
        None,
        "a pathname alone cannot establish the running image"
    );
    let link = dir.path().join("launch-link");
    std::os::unix::fs::symlink(&exe, &link).unwrap();
    let linked = Through::own(link.clone());
    assert_eq!(linked.unchanged(), Some(link.as_path()));
    let next_link = dir.path().join("next-link");
    std::os::unix::fs::symlink(dir.path().join("replacement"), &next_link).unwrap();
    std::fs::rename(next_link, &link).unwrap();
    assert_eq!(
        linked.unchanged(),
        None,
        "a retargeted launch link lost its image"
    );
    assert_eq!(Through::own(link).unchanged(), None);
}

/// The tie is lost once the file recorded to run through was replaced, or none read when it
/// was recorded, and never where nothing was recorded: a test binary's own spawns, which were
/// never tied there.
#[test]
fn a_tie_is_lost_only_once_its_recorded_executable_was_replaced() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let exe = dir.path().join("aterm");
    std::fs::write(&exe, b"the build this process started from").expect("write");
    assert!(!lost_of(None), "nothing recorded");
    let recorded = Some(Through::of(exe.clone()));
    assert!(!lost_of(Some(&recorded)), "the recorded file");
    let next = dir.path().join("aterm.next");
    std::fs::write(&next, b"an update's build").expect("write");
    std::fs::rename(&next, &exe).expect("rename over");
    assert!(lost_of(Some(&recorded)), "another file at its path");
    assert!(lost_of(Some(&None)), "nothing read when it was recorded");
}

/// One `/proc/<pid>/maps` line names the file it maps only at an address in its range.
#[test]
fn a_maps_line_names_its_file_at_an_address_in_its_range() {
    let line = "aaaab0000000-aaaab0100000 r-xp 00000000 103:02 4456      /usr/bin/aterm";
    assert_eq!(
        super::maps_file_at(line, 0xaaaa_b000_0010),
        Some(("103:02", 4456))
    );
    assert_eq!(
        super::maps_file_at(line, 0xaaaa_b010_0000),
        None,
        "past its end"
    );
    assert_eq!(
        super::maps_file_at("7f0000-7f1000 rw-p 00000000 00:00 0", 0x7f_0000),
        Some(("00:00", 0)),
        "no file"
    );
    assert_eq!(super::maps_file_at("7f0000-7f1000 r--p", 0x7f_0000), None);
}

/// Run through its dynamic loader (`ld.so <program>`), a process's `/proc/self/exe` is the
/// loader, which an exec of it would run with the tether's words for a program to load: such a
/// process does not run its own image, so it records nothing to run through; run directly, it
/// does. Negative control: answer `true` from `runs_its_own_image` before its maps read, and
/// the run through the loader says `true`.
#[cfg(target_os = "linux")]
#[test]
fn a_process_run_through_its_loader_does_not_run_its_own_image() {
    const SAY: &str = "ATERM_TETHER_SAY_OWN";
    const NAME: &str = "tether::tests::a_process_run_through_its_loader_does_not_run_its_own_image";
    if std::env::var_os(SAY).is_some() {
        println!("\nTETHER_OWN={}", super::runs_its_own_image());
        return;
    }
    let me = std::env::current_exe().expect("this test binary");
    // A statically linked test binary names no loader to run it through.
    let Some(loader) = interpreter_of(&me) else {
        return;
    };
    let run = |via: Option<&std::path::Path>| {
        let mut cmd = crate::spawnfd::command(via.unwrap_or(&me));
        if via.is_some() {
            cmd.arg(&me);
        }
        cmd.args([NAME, "--exact", "--nocapture", "--test-threads=1"])
            .env(SAY, "1");
        let out = crate::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("run this test binary");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("TETHER_OWN=").map(str::to_owned))
    };
    assert_eq!(run(None).as_deref(), Some("true"), "run directly");
    assert_eq!(
        run(Some(&loader)).as_deref(),
        Some("false"),
        "run through {}",
        loader.display()
    );
}

/// The loader an ELF64 little-endian `exe` names (`PT_INTERP`), read from its first MiB.
#[cfg(target_os = "linux")]
fn interpreter_of(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut image = Vec::new();
    std::fs::File::open(exe)
        .ok()?
        .take(1 << 20)
        .read_to_end(&mut image)
        .ok()?;
    let at = |from: usize, len: usize| image.get(from..from.checked_add(len)?);
    let word = |from: usize| Some(u64::from_le_bytes(at(from, 8)?.try_into().ok()?));
    let half = |from: usize| Some(u16::from_le_bytes(at(from, 2)?.try_into().ok()?));
    if at(0, 5)? != b"\x7fELF\x02" {
        return None;
    }
    let table = usize::try_from(word(0x20)?).ok()?;
    let size = usize::from(half(0x36)?);
    (0..usize::from(half(0x38)?)).find_map(|n| {
        let header = table.checked_add(n.checked_mul(size)?)?;
        // `PT_INTERP`.
        if at(header, 4)? != 3_u32.to_le_bytes() {
            return None;
        }
        let from = usize::try_from(word(header + 8)?).ok()?;
        let len = usize::try_from(word(header + 0x20)?).ok()?;
        let path = at(from, len)?.split(|&b| b == 0).next()?;
        Some(std::ffi::OsStr::from_bytes(path).into())
    })
}

/// The kernel signals a group by its leader's pidfd (Linux 6.9) while a member is left and the
/// leader was reaped, its number held by that member as its group's, and answers `ESRCH` once
/// none is: the watcher's census and signal by the kernel's record of the group, never its
/// number (`group::GroupPidfd`). A kernel with no such scope has nothing of it to check (its
/// watchers hold their groups by anchors, which `tether_live`'s `_by_listing` cases run).
/// Negative control: read the census of `ESRCH` as `Unknown`, and the emptied group never
/// reads empty.
#[cfg(target_os = "linux")]
#[test]
fn a_group_is_signalled_by_its_leaders_pidfd_after_the_leader_is_reaped() {
    use super::group::{GroupControl as _, GroupPidfd, Live};
    use std::os::fd::AsRawFd as _;
    if !super::signals_groups_by_pidfd() {
        return;
    }
    let mut leader = crate::spawnfd::command("/bin/sh");
    leader
        .args(["-c", "/bin/sleep 600 </dev/null >/dev/null 2>&1 &"])
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .process_group(0);
    let mut leader = leader.spawn().expect("start sh at the head of its group");
    // Unreaped, so the pid still names it.
    let pgid = i32::try_from(leader.id()).expect("a pid");
    let pidfd = super::pidfd_open(pgid, 0).expect("the leader's pidfd");
    let _ends = KilledByPidfd(pidfd.as_raw_fd());
    assert_eq!(
        super::child_exit_notice(&pidfd, std::time::Duration::from_secs(60)),
        super::ChildExitNotice::Exited,
        "sh exited within a minute"
    );
    assert!(leader.wait().expect("reap the leader").success());
    let mut group = GroupPidfd {
        parent: -1,
        leader: pidfd.as_raw_fd(),
        pgid,
        whole: false,
    };
    assert_eq!(group.census(pgid), Live::Unknown, "the sleep is left");
    group
        .signal_group(pgid, libc::SIGKILL)
        .expect("SIGKILL the group by its leader's pidfd");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while group.census(pgid) != Live::Empty {
        assert!(
            std::time::Instant::now() < until,
            "the killed sleep left the group within a minute"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        group.signal_group(pgid, 0).map_err(|e| e.raw_os_error()),
        Err(Some(libc::ESRCH))
    );
}

/// A group left with an exited leader alone, a zombie its parent has not reaped, holds a
/// process the kernel counts (signal `0` by its pidfd answers), which can fork nothing: where a
/// listing can be whole, the census reads it empty by two listings, which pass zombies by, so a
/// watcher waits for no reap. Negative control: drop the listings from `GroupPidfd::census`,
/// and the zombie keeps it `Unknown`.
#[cfg(target_os = "linux")]
#[test]
fn a_group_left_with_a_zombie_alone_reads_empty_by_its_listings() {
    use super::group::{GroupControl as _, GroupPidfd, Live};
    use std::os::fd::AsRawFd as _;
    if !super::signals_groups_by_pidfd() {
        return;
    }
    let mut leader = crate::spawnfd::command("/bin/true");
    leader
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .process_group(0);
    let mut leader = leader
        .spawn()
        .expect("start /bin/true at the head of its group");
    let pgid = i32::try_from(leader.id()).expect("a pid");
    let pidfd = super::pidfd_open(pgid, 0).expect("the leader's pidfd");
    assert_eq!(
        super::child_exit_notice(&pidfd, std::time::Duration::from_secs(60)),
        super::ChildExitNotice::Exited,
        "/bin/true exited within a minute"
    );
    assert!(
        super::group::signal_by_pidfd(pidfd.as_raw_fd(), 0).is_ok(),
        "the kernel counts the unreaped leader"
    );
    let whole = crate::process::group_listing_can_be_whole();
    let mut group = GroupPidfd {
        parent: -1,
        leader: pidfd.as_raw_fd(),
        pgid,
        whole,
    };
    assert_eq!(
        group.census(pgid),
        if whole { Live::Empty } else { Live::Unknown },
        "a zombie alone, where a listing can be whole: {whole}"
    );
    assert!(leader.wait().expect("reap the leader").success());
    assert_eq!(group.census(pgid), Live::Empty, "nothing left once reaped");
}

/// `SIGKILL` on drop to the group a pidfd's process led, by the kernel's record of it, so a
/// failing test leaves nothing it started and reaches no other group.
#[cfg(target_os = "linux")]
struct KilledByPidfd(i32);

#[cfg(target_os = "linux")]
impl Drop for KilledByPidfd {
    fn drop(&mut self) {
        let _ = super::group::signal_by_pidfd(self.0, libc::SIGKILL);
    }
}
