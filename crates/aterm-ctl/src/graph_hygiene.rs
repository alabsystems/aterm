// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What discovery (`instances` / `ls` / `windows`, and [`crate::fleet_sessions`])
//! does NOT dial, and the one tidy-up it does on the way.
//!
//! WHY: an orphaned private headless instance mirrors its graph entry into
//! the rendezvous directory while its socket and token live elsewhere, so
//! every `aterm ctl ls` dialed it and was refused (a `DENIED` WARN in its log
//! per call). Two rules, both on the client:
//!
//! 1. **A graph entry whose hosting pid is dead is skipped without a connect**
//!    and removed ([`prune_dead_entry`]): nothing can be serving it, and the
//!    entry would otherwise be re-read on every call. The removal is the ONLY
//!    act, and it is guarded so it can never delete an entry a live instance
//!    just rewrote. A pid that is alive but names a process YOUNGER than the
//!    entry ([`writer_gone`]) is skipped the same way — the writer died and its
//!    pid was handed on (pids wrap here in about ten minutes), so a stale entry
//!    would otherwise describe, and `instances` advise killing, a stranger — but
//!    it is NOT removed: that judgement rests on the wall clock, which can be
//!    stepped back under a live writer.
//! 2. **An instance this client cannot authenticate to is FOREIGN**
//!    ([`foreign_token_miss`]): its socket lives outside the rendezvous
//!    directory (a private `--control-sock`) AND no token file beside it can be
//!    read. The request could only be refused, so it is never sent; the
//!    instance is reported as foreign (`instances` says so in one stderr line,
//!    and a fleet with nothing else in it names it in the report) instead of
//!    being dialed on every call.
//!
//! NEVER HIDDEN: an instance whose token this client CAN read is probed,
//! wherever its socket lives, and every socket in the
//! rendezvous directory itself is probed whatever its token says — a miss
//! there is a problem with the owner's own instance, and the report names it.
//! `--sock` / `--pid` still dial what they name: an explicit target is the
//! caller's decision, not discovery's.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aterm_uds::process::ProcessBirth;

/// How far a writer's recorded birth may trail its entry's mtime and still be
/// its writer. Both are wall-clock readings taken by different kernel paths, an
/// mtime is only as fine as its filesystem keeps it (HFS+ 1 s, FAT 2 s), and a
/// clock can be set back a little between the launch and the write; without
/// this an instance writing its entry in its first moments could read as
/// younger than it and have its live entry pruned. The window costs nothing in
/// the other direction: a reused pid born inside it needs the pid counter to
/// wrap in two seconds, and it takes about ten minutes here. The slack of a
/// kernel wall-clock record (macOS); Linux places its birth (`PLACED_SLACK`).
#[cfg(any(not(any(target_os = "linux", target_os = "android")), test))]
pub(crate) const WRITE_SLACK: Duration = Duration::from_secs(2);

/// `WRITE_SLACK` for a birth PLACED on the wall clock from the boot clock
/// ([`read_birth`] on Linux): the placement uses this client's clocks now, so a
/// wall-clock step made since the writer wrote moves it by the step. A minute
/// absorbs any step short of a manual reset and costs nothing in the other
/// direction: a reused pid born inside it needs the pid counter to wrap within
/// a minute of the write, and Linux's `pid_max` (4194304 here) makes that far
/// rarer than macOS's ten minutes. Linux only: nothing else places a birth.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) const PLACED_SLACK: Duration = Duration::from_secs(60);

/// A live process's start on the wall clock ([`read_birth`]), with the slack its
/// writer judgement allows ([`is_entry_writer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Birth {
    pub(crate) at: ProcessBirth,
    pub(crate) slack: Duration,
}

/// The clocks [`read_birth`] places a Linux boot-clock start by: ONE paired
/// reading of the wall and boot clocks, taken at the first read through it,
/// after that read's start (so the process read is never younger than the
/// pairing). Every read through one `Clocks` places one process at one instant,
/// so two reads of it agree exactly; a judgement takes a fresh one, so a client
/// that lives on (the fleet bridge's repeated discovery, `aterm fleet events`)
/// still places a process born after its earlier judgements — a start later
/// than a pairing places nowhere. Elsewhere it holds nothing: macOS keeps the
/// wall-clock record itself.
#[derive(Debug, Default)]
pub(crate) struct Clocks {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    paired: std::cell::OnceCell<Option<(SystemTime, aterm_uds::process::BootClock)>>,
}

/// When the live process `pid` of this uid started, on the wall clock, or `None`
/// where this client cannot say (another uid's process, an ended one, a
/// platform with neither record).
///
/// macOS keeps the wall-clock record itself (`read_process_birth`, exact,
/// `WRITE_SLACK`). Linux keeps only a boot-clock start (`/proc/<pid>/stat`
/// `starttime`, `read_process_start`): it is placed on the wall clock by the
/// paired reading `clocks` holds, so two reads of one process through one
/// [`Clocks`] agree exactly (`PLACED_SLACK`). Until 2026-10-16 Linux answered
/// `None` here, and a graph entry whose pid had been handed to another process
/// was listed by `aterm ctl instances` as a phantom private instance.
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

/// A boot-clock start (Linux nanoseconds) on the wall clock, by the paired reading
/// `wall` / `boot` taken at one moment: `wall - (boot - start)`. `None` for a start
/// after the reading or one that does not fit. Pure.
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

/// Whether the LIVE process born at `birth` is the one that wrote a graph entry
/// naming its pid, last written at `written`.
///
/// `Some(true)`: born no later than the write (`slack` aside, `WRITE_SLACK` or
/// `PLACED_SLACK`). Alive now, it held that pid at the write, and no two
/// processes hold one pid at once, so it is the writer. `Some(false)`: born after
/// the entry was last written, it cannot have written it — the writer is gone and
/// the pid was handed on. `None`: the times do not compare (a birth past what
/// `SystemTime` holds), and nothing is claimed either way.
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

/// Whether the instance that wrote a graph entry naming `pid` (non-zero), last
/// written at `written`, is provably gone: the pid is dead, or it now names a
/// process younger than the entry ([`is_entry_writer`]). A birth this client
/// cannot read ([`read_birth`]: another uid's process, a sandbox refusing
/// `proc_pidinfo`, a platform with no start record) proves nothing, and the
/// entry is kept as before.
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

/// A graph entry's body and when it was last written, from ONE open. The mtime
/// is read AFTER the body, so it is never older than the write that produced
/// the bytes read (a writer truncates and rewrites in place, so a rewrite in
/// between only makes it newer, which can only keep an entry). `None` for the
/// mtime where the filesystem keeps none.
pub(crate) fn read_entry(path: &Path) -> std::io::Result<(String, Option<SystemTime>)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut body = String::new();
    file.read_to_string(&mut body)?;
    let written = file.metadata().and_then(|m| m.modified()).ok();
    Ok((body, written))
}

/// Remove the graph entry at `entry`, whose body read `seen` and named a
/// hosting pid whose writer is gone ([`writer_gone`]) — but only if it STILL
/// says exactly that. Returns whether the entry was removed.
///
/// The entry is renamed aside (atomic within the directory) and read back:
///
/// * the same bytes: the dead instance's entry, removed;
/// * anything else: a live instance rewrote this sid in between (a handoff
///   successor republishing its session). It goes back under its name unless
///   a newer entry has landed there (`hard_link` refuses to replace one).
///
/// A writer that opened the entry before the rename writes into the renamed
/// file, so it no longer holds `seen` and is put back. Best-effort: a failed
/// rename or read leaves everything as it was.
pub(crate) fn prune_dead_entry(entry: &Path, seen: &str) -> bool {
    let (Some(dir), Some(name)) = (entry.parent(), entry.file_name()) else {
        return false;
    };
    // A dot-name no graph reader takes for a sid (a sid is `s-<hex>`), unique
    // to this process so two concurrent pruners cannot collide on it.
    let mut aside_name = std::ffi::OsString::from(".prune-");
    aside_name.push(name);
    aside_name.push(format!("-{}", std::process::id()));
    let aside = dir.join(aside_name);
    if std::fs::rename(entry, &aside).is_err() {
        return false;
    }
    let unchanged = std::fs::read_to_string(&aside).is_ok_and(|now| now == seen);
    if !unchanged {
        // Rewritten under us: restore it, unless a newer entry already holds
        // the name (then that one is the live answer and ours is older).
        let _ = std::fs::hard_link(&aside, entry);
    }
    let _ = std::fs::remove_file(&aside);
    unchanged
}

/// `Some(token path looked for)` when the instance at `sock` is foreign to a
/// client reading the rendezvous directory `dir`: its socket lives outside
/// `dir` and no token beside it can be read ([`crate::read_token_at`]).
/// `None` (dial it) for a socket inside `dir` or one whose token reads.
pub(crate) fn foreign_token_miss(dir: &Path, sock: &str) -> Option<PathBuf> {
    if inside(dir, Path::new(sock)) {
        return None;
    }
    crate::read_token_at(sock).err().map(|(path, _)| path)
}

/// Whether `sock`'s parent directory IS `dir`, through any symlinked ancestor
/// (macOS `/var` → `/private/var`). Canonical when both resolve, else lexical:
/// a false "inside" only means the socket is probed as it always was.
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
    /// survives byte for byte. Control: the same call with the body it now
    /// holds removes it, leaving nothing aside.
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

    /// The identity rule's edges: born before the write is the writer, born
    /// inside the slack after it still is, a microsecond past the slack is not,
    /// and a birth `SystemTime` cannot hold claims nothing.
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
    /// Two reads of one process through one pairing agree exactly. The 20 ms
    /// gap covers two Linux ticks of `starttime` truncation.
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

    /// Only a socket OUTSIDE the directory with NO readable token is foreign:
    /// an in-directory socket is the owner's own (its miss is reported, not
    /// skipped), and an outside one whose token reads is reachable.
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
