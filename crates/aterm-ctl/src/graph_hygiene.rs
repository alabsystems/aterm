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
/// wrap in two seconds, and it takes about ten minutes here.
const WRITE_SLACK: Duration = Duration::from_secs(2);

/// Whether the LIVE process born at `birth` is the one that wrote a graph entry
/// naming its pid, last written at `written`.
///
/// `Some(true)`: born no later than the write ([`WRITE_SLACK`] aside). Alive
/// now, it held that pid at the write, and no two processes hold one pid at
/// once, so it is the writer. `Some(false)`: born after the entry was last
/// written, it cannot have written it — the writer is gone and the pid was
/// handed on. `None`: the times do not compare (a birth past what `SystemTime`
/// holds), and nothing is claimed either way.
pub(crate) fn is_entry_writer(birth: ProcessBirth, written: SystemTime) -> Option<bool> {
    let born = UNIX_EPOCH
        .checked_add(Duration::from_secs(birth.seconds))?
        .checked_add(Duration::from_micros(birth.microseconds))?;
    Some(born <= written.checked_add(WRITE_SLACK)?)
}

/// Whether the instance that wrote a graph entry naming `pid` (non-zero), last
/// written at `written`, is provably gone: the pid is dead, or it now names a
/// process younger than the entry ([`is_entry_writer`]). A birth this client
/// cannot read — another uid's process, a sandbox refusing `proc_pidinfo`, a
/// platform that keeps no wall-clock birth — proves nothing, and the entry is
/// kept as before.
pub(crate) fn writer_gone(pid: u32, written: Option<SystemTime>) -> bool {
    if !aterm_uds::process::pid_alive(pid) {
        return true;
    }
    let birth = i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_process_birth);
    matches!(
        (birth, written),
        (Some(birth), Some(written)) if is_entry_writer(birth, written) == Some(false)
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

    /// The entry still says what was read: removed, and nothing is left aside.
    #[test]
    fn prune_removes_an_unchanged_dead_entry_and_leaves_nothing_aside() {
        let s = scratch();
        let entry = s.path().join("s-0bad");
        let body = "sock /tmp/x/a.sock\nnonce 00\npid 1234567\n";
        std::fs::write(&entry, body).unwrap();
        assert!(prune_dead_entry(&entry, body));
        assert!(!entry.exists());
        assert_eq!(
            std::fs::read_dir(s.path()).unwrap().count(),
            0,
            "no aside file"
        );
    }

    /// A live instance rewrote the sid after it was read dead: the rewrite
    /// survives byte for byte. Control: the same call with the body it now
    /// holds removes it.
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
        assert_eq!(is_entry_writer(at(1_789_999_000, 0), written), Some(true));
        assert_eq!(
            is_entry_writer(at(1_790_000_002, 0), written),
            Some(true),
            "inside the slack"
        );
        assert_eq!(
            is_entry_writer(at(1_790_000_002, 1), written),
            Some(false),
            "a microsecond past it"
        );
        assert_eq!(is_entry_writer(at(u64::MAX, 0), written), None);
    }

    /// Live, on this test process: a dead pid's writer is gone; a live pid
    /// older than its entry, or one with no entry time to judge, is kept; and
    /// a live pid YOUNGER than its entry is gone wherever the kernel keeps the
    /// wall-clock birth (macOS) — elsewhere it proves nothing and is kept.
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
            cfg!(target_vendor = "apple"),
            "an entry written before this process was born is not its"
        );
    }

    /// The body and the mtime come from the same file.
    #[test]
    fn read_entry_returns_the_body_and_its_last_write() {
        let s = scratch();
        let entry = s.path().join("s-0bad");
        std::fs::write(&entry, "sock /tmp/x/a.sock\npid 7\n").unwrap();
        let old = UNIX_EPOCH + Duration::from_secs(1_000);
        std::fs::File::options()
            .write(true)
            .open(&entry)
            .and_then(|f| f.set_modified(old))
            .unwrap();
        let (body, written) = read_entry(&entry).unwrap();
        assert_eq!(body, "sock /tmp/x/a.sock\npid 7\n");
        assert_eq!(written, Some(old));
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
