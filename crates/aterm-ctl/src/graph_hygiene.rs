// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Discovery graph hygiene: dead-writer pruning and foreign-socket filtering
//! ([`crate::fleet_sessions`], [`prune_dead_entry`], [`writer_gone`], [`foreign_token_miss`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aterm_uds::process::ProcessBirth;

/// How far a writer's recorded birth may trail its entry's mtime and still be its writer.
#[cfg(any(not(any(target_os = "linux", target_os = "android")), test))]
pub(crate) const WRITE_SLACK: Duration = Duration::from_secs(2);

/// `WRITE_SLACK` for a boot-clock birth placed on the wall clock ([`read_birth`] on Linux).
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) const PLACED_SLACK: Duration = Duration::from_secs(60);

/// A live process's start on the wall clock ([`read_birth`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Birth {
    pub(crate) at: ProcessBirth,
    pub(crate) slack: Duration,
}

/// The clocks [`read_birth`] places a Linux boot-clock start on.
#[derive(Debug, Default)]
pub(crate) struct Clocks {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    paired: std::cell::OnceCell<Option<(SystemTime, aterm_uds::process::BootClock)>>,
}

/// When the live process `pid` of this uid started, on the wall clock.
pub(crate) fn read_birth(pid: i32, clocks: &Clocks) -> Option<Birth> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let start = aterm_uds::process::read_process_start(pid)?;
        let (wall, boot) = (*clocks.paired.get_or_init(|| {
            let boot = aterm_uds::process::boot_clock_now()?;
            Some((SystemTime::now(), boot))
        }))?;
        place_on_wall(start.boot, wall, boot).map(|at| Birth {
            at,
            slack: PLACED_SLACK,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let Clocks {} = clocks;
        aterm_uds::process::read_process_birth(pid).map(|at| Birth {
            at,
            slack: WRITE_SLACK,
        })
    }
}

/// A boot-clock start (Linux nanoseconds) on the wall clock.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
pub(crate) fn place_on_wall(
    start: aterm_uds::process::BootClock,
    wall: SystemTime,
    boot: aterm_uds::process::BootClock,
) -> Option<ProcessBirth> {
    let age = Duration::from_nanos(boot.0.checked_sub(start.0)?);
    let born = wall.checked_sub(age)?.duration_since(UNIX_EPOCH).ok()?;
    Some(ProcessBirth {
        seconds: born.as_secs(),
        microseconds: u64::from(born.subsec_micros()),
    })
}

/// Whether the live process born at `birth` wrote a graph entry naming its pid.
pub(crate) fn is_entry_writer(
    birth: ProcessBirth,
    slack: Duration,
    written: SystemTime,
) -> Option<bool> {
    let born = UNIX_EPOCH
        .checked_add(Duration::from_secs(birth.seconds))?
        .checked_add(Duration::from_micros(birth.microseconds))?;
    Some(born <= written.checked_add(slack)?)
}

/// Whether the instance that wrote a graph entry naming `pid` (non-zero) is gone.
pub(crate) fn writer_gone(pid: u32, written: Option<SystemTime>) -> bool {
    if !aterm_uds::process::pid_alive(pid) {
        return true;
    }
    let birth = i32::try_from(pid)
        .ok()
        .and_then(|pid| read_birth(pid, &Clocks::default()));
    matches!(
        (birth, written),
        (Some(birth), Some(written))
            if is_entry_writer(birth.at, birth.slack, written) == Some(false)
    )
}

/// A graph entry's body and when it was last written, from one open.
pub(crate) fn read_entry(path: &Path) -> std::io::Result<(String, Option<SystemTime>)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut body = String::new();
    file.read_to_string(&mut body)?;
    let written = file.metadata().and_then(|m| m.modified()).ok();
    Ok((body, written))
}

/// Remove the graph entry at `entry`.
pub(crate) fn prune_dead_entry(entry: &Path, seen: &str) -> bool {
    let (Some(dir), Some(name)) = (entry.parent(), entry.file_name()) else {
        return false;
    };
    let mut aside_name = std::ffi::OsString::from(".prune-");
    aside_name.push(name);
    aside_name.push(format!("-{}", std::process::id()));
    let aside = dir.join(aside_name);
    if std::fs::rename(entry, &aside).is_err() {
        return false;
    }
    let unchanged = std::fs::read_to_string(&aside).is_ok_and(|now| now == seen);
    if !unchanged {
        // the name (then that one is the live answer and ours is older).
        let _ = std::fs::hard_link(&aside, entry);
    }
    let _ = std::fs::remove_file(&aside);
    unchanged
}

/// `Some(token_path)` when the instance at `sock` is foreign to the rendezvous directory.
pub(crate) fn foreign_token_miss(dir: &Path, sock: &str) -> Option<PathBuf> {
    if inside(dir, Path::new(sock)) {
        return None;
    }
    crate::read_token_at(sock).err().map(|(path, _)| path)
}

/// Whether `sock`'s parent directory is `dir`, through any symlinked ancestor
/// (macOS `/var` → `/private/var`).
fn inside(dir: &Path, sock: &Path) -> bool {
    let Some(parent) = sock.parent() else {
        return false;
    };
    match (std::fs::canonicalize(parent), std::fs::canonicalize(dir)) {
        (Ok(a), Ok(b)) => a == b,
        _ => parent == dir,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A scratch dir removed on drop, short enough for socket paths.
    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::TempDir::new_in("/tmp").expect("scratch dir")
    }

    /// A live instance rewrote the sid after it was read dead: the rewrite
    /// survives byte for byte.
    #[test]
    fn prune_never_removes_an_entry_rewritten_after_it_was_read() {
        let s = scratch();
        let entry = s.path().join("s-0bad");
        let dead = "sock /tmp/x/a.sock\nnonce 00\npid 1234567\n";
        let live = format!("sock /tmp/x/a.sock\nnonce 11\npid {}\n", std::process::id());
        std::fs::write(&entry, &live).unwrap();
        assert!(!prune_dead_entry(&entry, dead));
        assert_eq!(std::fs::read_to_string(&entry).unwrap(), live);
        assert_eq!(
            std::fs::read_dir(s.path()).unwrap().count(),
            1,
            "no aside file"
        );
        assert!(
            prune_dead_entry(&entry, &live),
            "control: the guard, not a no-op"
        );
        assert_eq!(
            std::fs::read_dir(s.path()).unwrap().count(),
            0,
            "removed, and no aside file"
        );
    }

    /// [`is_entry_writer`]'s edges: before the write, inside the slack, a microsecond past it,
    /// and a birth `SystemTime` cannot hold.
    #[test]
    fn a_writer_is_born_no_later_than_its_write_give_or_take_the_slack() {
        let written = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let at = |seconds, microseconds| ProcessBirth {
            seconds,
            microseconds,
        };
        assert_eq!(
            is_entry_writer(at(1_789_999_000, 0), WRITE_SLACK, written),
            Some(true)
        );
        assert_eq!(
            is_entry_writer(at(1_790_000_002, 0), WRITE_SLACK, written),
            Some(true),
            "inside the slack"
        );
        assert_eq!(
            is_entry_writer(at(1_790_000_002, 1), WRITE_SLACK, written),
            Some(false),
            "a microsecond past it"
        );
        assert_eq!(is_entry_writer(at(u64::MAX, 0), WRITE_SLACK, written), None);
    }

    /// A boot-clock start lands on the wall clock its age before the paired
    /// reading; a start after the reading lands nowhere.
    #[test]
    fn a_boot_clock_start_is_placed_its_age_before_the_paired_reading() {
        use aterm_uds::process::BootClock;
        let wall = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let boot = BootClock(5_000_000_000_000);
        assert_eq!(
            place_on_wall(BootClock(4_000_000_250_000), wall, boot),
            Some(ProcessBirth {
                seconds: 1_789_999_000,
                microseconds: 250,
            })
        );
        assert_eq!(
            place_on_wall(BootClock(5_000_000_000_001), wall, boot),
            None,
            "a start after the reading"
        );
    }

    /// Live, on this test process: a dead pid's writer is gone; a live pid
    /// older than its entry, or one with no entry time to judge, is kept; and
    /// a live pid YOUNGER than its entry is gone wherever this client reads a
    /// start (macOS's wall record, Linux's boot-clock one placed on the wall) —
    /// elsewhere it proves nothing and is kept.
    #[test]
    fn a_writer_is_gone_when_its_pid_is_dead_or_younger_than_its_entry() {
        let me = std::process::id();
        let now = SystemTime::now();
        assert!(writer_gone(0x7fff_fff0, Some(now)), "a dead pid");
        assert!(
            !writer_gone(me, Some(now)),
            "alive, and older than the entry"
        );
        assert!(!writer_gone(me, None), "no entry time proves nothing");
        let before_me = UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(
            writer_gone(me, Some(before_me)),
            cfg!(any(
                target_vendor = "apple",
                target_os = "linux",
                target_os = "android"
            )),
            "an entry written before this process was born is not its"
        );
    }

    /// A client that lives on (the fleet bridge's repeated discovery) still
    /// judges a pid born after its earlier judgements: each judgement places
    /// births by its own pairing of the clocks, where one pairing kept for the
    /// process's life placed every later start nowhere and left it unjudged.
    #[test]
    fn a_pid_born_after_an_earlier_judgement_is_still_judged() {
        let me = std::process::id();
        let own = i32::try_from(me).unwrap();
        let kept = Clocks::default();
        let first = read_birth(own, &kept);
        assert!(
            !writer_gone(me, Some(SystemTime::now())),
            "a first judgement"
        );
        std::thread::sleep(Duration::from_millis(20));
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        let pid = child.id();
        let gone = writer_gone(pid, Some(UNIX_EPOCH + Duration::from_secs(1_000)));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            gone,
            cfg!(any(
                target_vendor = "apple",
                target_os = "linux",
                target_os = "android"
            )),
            "a pid younger than its entry, born after the first judgement"
        );
        assert_eq!(read_birth(own, &kept), first, "one pairing, one instant");
    }

    /// Only a socket outside the directory with no readable token is foreign.
    #[test]
    fn foreign_is_outside_and_tokenless_only() {
        let s = scratch();
        let dir = s.path().join("rv");
        let private = s.path().join("priv");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        let outside = private.join("a.sock");
        let outside = outside.to_str().unwrap();
        assert_eq!(
            foreign_token_miss(&dir, outside),
            Some(private.join("a.sock.token")),
            "outside and tokenless: foreign, naming the file it looked for"
        );
        std::fs::write(private.join("a.sock.token"), "feedbeef\n").unwrap();
        assert_eq!(
            foreign_token_miss(&dir, outside),
            None,
            "token reads: reachable"
        );
        let own = dir.join("aterm-4242.sock");
        assert_eq!(
            foreign_token_miss(&dir, own.to_str().unwrap()),
            None,
            "inside the rendezvous dir: never foreign"
        );
    }
}
