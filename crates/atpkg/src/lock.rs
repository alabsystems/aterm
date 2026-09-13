// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The store-wide single-writer lock — atpkg's **single-writer-per-store contract**.
//!
//! The coherence-group transaction ([`crate::flow`]) stages into the SAME
//! `staging/<program>/` and `store/<program>/<build>/` paths in every process, and an
//! aborting transaction DISCARDS the build dirs it staged
//! ([`crate::store::discard_build`]). That discard — and the check-then-act windows in
//! [`crate::linkmode`] reconciliation and the pin file's read-modify-write — are only
//! sound while **one process at a time mutates a store**. Two concurrent mutators (the
//! GUI Settings ▸ Packages worker, the 6-hour update loop, a second aterm instance, a
//! manual CLI invocation) could otherwise have one process's abort delete the very
//! build the other just activated: dangling shims, a wedged coherence tuple.
//!
//! So every verb that MUTATES the store TRY-acquires this advisory lock — a
//! `store.lock` file (`0600`) directly under the hardened pkg prefix — at the CLI edge
//! ([`crate::cli::main_entry`]) and holds it for the whole verb. Contention is
//! **fail-closed and LOUD** for a HUMAN-typed verb: the refusal names the lock path
//! and exits [`CONTENDED_EXIT`] (75, `EX_TEMPFAIL` — distinct from the exit-1 `Io`
//! refusal, so a caller classifies by CODE and never by the sentence). MACHINE lanes
//! — the window's launch-time `seed` and `update` children — opt into a BOUNDED WAIT
//! instead with `--wait-lock <secs>` ([`lock_store_waiting`]): the same try, then a
//! 500 ms poll until the holder exits or the deadline passes, announced once on
//! stdout as the `lock-waiting:` marker after a short grace. That is what makes a
//! second aterm process mid-install — the macOS Full Disk Access grant, which quits
//! the app and opens it again; a self-update re-exec; a second window opened by
//! hand — CONTINUE the sibling's pass instead of reporting it as a failed install
//! (incident 2026-09-10: the second instance was refused 13 s after launch, showed
//! "⚠ ALab toolchain install failed", and nothing retried for 6 h). Waiting on a
//! holder that may DIE is safe: the store is crash-consistent at every point
//! (`install.rs` stages into `.incoming-<pid>` scratch beside the live tree,
//! `store.rs` recovers an interrupted two-rename swap and sweeps leftover scratch,
//! `flow.rs` aborts by discarding only never-live builds, `gc.rs` sweeps under a
//! claim guard, `.part` downloads resume, and `progress.rs`'s dead-pid rule retires
//! a dead writer's liveness claim) and the kernel releases the flock with the
//! holder. Read-only verbs never touch the lock.
//!
//! The TRY semantics are why this does not reuse `aterm_update_core::FileLock`
//! directly: that primitive's `acquire` BLOCKS (`flock(LOCK_EX)`) — right for the
//! floor file's millisecond critical section ([`crate::sig::Floor`]), wrong for a
//! whole multi-minute install that must refuse, not queue. This is the same std
//! `flock` wrapper (`File::try_lock` IS `flock(fd, LOCK_EX | LOCK_NB)` on Unix,
//! `LockFileEx(LOCKFILE_FAIL_IMMEDIATELY)` on Windows) with the non-blocking flag,
//! and the same open discipline as that primitive (create `0600`, never truncate —
//! a lock file is a rendezvous, not data; std opens close-on-exec by default). The
//! waiting form is a poll over the same try, never a blocking `flock`.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::store::Layout;

/// The store-wide writer lock, held for the lifetime of the value. Dropping it (or
/// the process exiting) releases it — `flock` is associated with the open file
/// description, so the kernel always cleans up after a crashed holder.
pub struct StoreLock {
    _file: File,
}

/// Why the store lock could not be taken. Both variants are refusals at the CLI
/// edge (fail-closed): a mutating verb never proceeds without the lock.
#[derive(Debug)]
pub enum StoreLockError {
    /// Another atpkg process holds the lock (the one-line loud refusal, §single-writer).
    Contended(PathBuf),
    /// The lock file could not be created/opened/locked for a non-contention reason
    /// (unwritable prefix, I/O error). Still a refusal — mutating without the lock
    /// would silently reopen the concurrent-transaction hazard.
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for StoreLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreLockError::Contended(path) => write!(
                f,
                "another atpkg process holds the store lock at {} — refusing to mutate \
                 the store concurrently (retry when it exits)",
                path.display()
            ),
            // A PERMISSION failure and an ordinary I/O failure have opposite
            // remedies and used to share one message. The permission case is the
            // common one and it has a name: the configured prefix belongs to
            // somebody else. Saying only "Operation not permitted (os error 1)"
            // left a reader to guess, and the guess that got written down was
            // "atpkg structurally requires sudo" — it does not; the default prefix
            // is $HOME-owned and needs no privilege
            // (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md).
            StoreLockError::Io(path, e) if e.kind() == io::ErrorKind::PermissionDenied => {
                write!(
                    f,
                    "cannot take the store lock at {}: {e} — refusing to mutate the \
                     store without it. This prefix is not writable by you; atpkg's \
                     default prefix is under $HOME and needs no privilege, so remove \
                     `[packages].prefix` from ~/.config/aterm/aterm.toml unless you \
                     really want a shared multi-user store (then install as its owner)",
                    path.display()
                )
            }
            StoreLockError::Io(path, e) => write!(
                f,
                "cannot take the store lock at {}: {e} — refusing to mutate the store \
                 without it",
                path.display()
            ),
        }
    }
}

impl std::error::Error for StoreLockError {}

/// The exit code a CLI-edge CONTENTION refusal carries: sysexits `EX_TEMPFAIL` (75),
/// the one code atpkg reserves for "try again later", so a machine caller — the
/// window's launch lanes, `install.sh` — classifies by CODE and never string-matches
/// the sentence. `Io` refusals keep exit 1: an unwritable prefix is not transient.
/// (The other codes in use are 1, 2, 3 and 127; none of them may mean this.)
pub const CONTENDED_EXIT: u8 = 75;

/// How often a waiting acquisition re-tries the lock. A re-try is a fresh
/// [`try_lock_store`] — `ensure_dir` plus one open — on purpose: an fd held across a
/// prefix that was removed and re-created under it would "acquire" an orphan inode
/// while a new process locks the real file, and a few syscalls every half second
/// is nothing against a wait that is idle by definition.
const WAIT_POLL: Duration = Duration::from_millis(500);

/// How long a waiter stays SILENT before it announces the wait to its caller
/// (`on_first_contention`). Every window's no-op update tick holds the lock for the
/// seconds an index fetch takes — at every launch and every 6 h — and a second
/// process that overlaps one of those must not put "waiting for another install" on
/// the glass and re-grid twice for a pass that installed nothing (the status bars'
/// no-op rule). The incident's gap was 13 s; a real install holds the lock for
/// minutes.
pub const WAIT_ANNOUNCE_GRACE: Duration = Duration::from_secs(2);

/// The longest wait a caller can ask for. A `--wait-lock` value is parsed from argv,
/// and `u64::MAX` seconds must not overflow the deadline arithmetic.
const WAIT_MAX: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Acquire the store-wide writer lock, WAITING up to `timeout` for a holder to
/// release it: the same [`try_lock_store`] first, then a [`WAIT_POLL`] cadence until
/// it succeeds, the deadline passes (`Err(Contended)` — the loud sentence, unchanged),
/// or `still_wanted` says the caller has gone (also `Err(Contended)`, so a waiter
/// whose window quit stands down instead of racing the successor's own waiter for
/// the freed lock). `on_first_contention` fires ONCE, with the lock path, the first
/// time the wait has outlasted [`WAIT_ANNOUNCE_GRACE`] — the `lock-waiting:` marker
/// the window turns into its waiting row — and `still_wanted` is consulted BEFORE
/// it on every poll, so a caller that is already gone is never announced to: its
/// stdout is the dead window's pipe, and a print there is EPIPE, which is not the
/// silent stand-down the caller was promised. An `Io` refusal is returned
/// immediately: an unwritable prefix is not something to wait on. `timeout == 0`
/// is one silent try. Polls rather than `flock(LOCK_EX)` so `try_lock_store` stays
/// the one primitive and Windows keeps its `FAIL_IMMEDIATELY` parity.
pub fn lock_store_waiting(
    layout: &Layout,
    timeout: Duration,
    on_first_contention: impl FnOnce(&Path),
    still_wanted: impl Fn() -> bool,
) -> Result<StoreLock, StoreLockError> {
    let started = Instant::now();
    let deadline = started + timeout.min(WAIT_MAX);
    let mut announce = Some(on_first_contention);
    loop {
        match try_lock_store(layout) {
            Ok(guard) => return Ok(guard),
            Err(StoreLockError::Contended(path)) => {
                // The orphan check comes FIRST: a waiter whose caller has gone
                // stands down without a word, whether or not the grace has
                // passed (its caller's pipe is exactly what it must not print to).
                if !still_wanted() {
                    return Err(StoreLockError::Contended(path));
                }
                let now = Instant::now();
                if now.duration_since(started) >= WAIT_ANNOUNCE_GRACE
                    && let Some(announce) = announce.take()
                {
                    announce(&path);
                }
                if now >= deadline {
                    return Err(StoreLockError::Contended(path));
                }
                std::thread::sleep(WAIT_POLL.min(deadline.saturating_duration_since(now)));
            }
            Err(io) => return Err(io),
        }
    }
}

/// TRY-acquire the store-wide writer lock for `layout`'s store. Never blocks: a held
/// lock is [`StoreLockError::Contended`] immediately. Creates the (vetted) prefix
/// `0700` first — the lock must be takeable before a first install has built the
/// store — exactly as every other prefix writer does ([`crate::pin::set_pinned`]).
///
/// Factored off the CLI edge so lock contention is unit-testable in-process: two
/// `Layout`s over one prefix contend exactly like two processes do (`flock` treats
/// separate open file descriptions independently, same-process or not).
pub fn try_lock_store(layout: &Layout) -> Result<StoreLock, StoreLockError> {
    let path = layout.store_lock();
    if let Err(e) = layout.ensure_dir(&layout.prefix) {
        return Err(StoreLockError::Io(path, e));
    }
    let mut opts = std::fs::OpenOptions::new();
    // Never truncate: a lock file is a rendezvous, not data (the FileLock discipline).
    opts.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let file = match opts.open(&path) {
        Ok(f) => f,
        Err(e) => return Err(StoreLockError::Io(path, e)),
    };
    match file.try_lock() {
        Ok(()) => Ok(StoreLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(StoreLockError::Contended(path)),
        Err(std::fs::TryLockError::Error(e)) => Err(StoreLockError::Io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-lock-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Layout { prefix: p }
    }

    /// The single-writer contract, in-process: with the lock held (one `Layout`),
    /// a second acquisition over the SAME prefix (a second `Layout`, a distinct
    /// open file description — exactly what a second process presents) refuses
    /// IMMEDIATELY and LOUDLY, naming the lock path and the holder; dropping the
    /// guard releases it for the next acquisition.
    #[test]
    fn contended_store_lock_refuses_loudly_and_releases_on_drop() {
        let a = temp_layout("contend");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let err = match try_lock_store(&b) {
            Ok(_) => panic!("a held store lock must refuse a second mutator"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Contended(ref p) if *p == a.store_lock()),
            "contention names the lock path: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("another atpkg process holds the store lock"),
            "the refusal names the holder: {msg}"
        );
        assert!(
            msg.contains(&a.store_lock().display().to_string()),
            "the refusal names the lock path: {msg}"
        );
        drop(guard);
        let reacquired = try_lock_store(&b).expect("released lock is takeable again");
        drop(reacquired);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// First contact: the prefix does not exist yet (nothing installed) — the lock
    /// must still be takeable (the first `install` needs it), and both the created
    /// prefix and the lock file carry the hardened modes.
    #[test]
    fn store_lock_creates_the_prefix_and_hardens_the_lock_file() {
        let l = temp_layout("fresh");
        assert!(!l.prefix.exists(), "fixture: no prefix yet");
        let guard = try_lock_store(&l).expect("lockable before the store exists");
        assert!(
            l.store_lock().is_file(),
            "store.lock created under the prefix"
        );
        #[cfg(unix)]
        {
            let dir_mode = std::fs::metadata(&l.prefix).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "prefix hardened 0700");
            let mode = std::fs::metadata(l.store_lock())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "store.lock is 0600");
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// An unusable prefix (a plain FILE where the directory should be) is a
    /// fail-closed `Io` refusal, never a silent lock-free pass.
    #[test]
    fn unusable_prefix_refuses_instead_of_passing_lock_free() {
        let l = temp_layout("badprefix");
        std::fs::write(&l.prefix, b"not a directory").unwrap();
        let err = match try_lock_store(&l) {
            Ok(_) => panic!("a file-shaped prefix cannot yield a store lock"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Io(..)),
            "refusal is Io: {err:?}"
        );
        assert!(
            err.to_string().contains("refusing to mutate the store"),
            "the Io refusal is fail-closed too: {err}"
        );
        let _ = std::fs::remove_file(&l.prefix);
    }

    /// The waiting form (the launch lanes' `--wait-lock`, 2026-09-10): a holder that
    /// releases inside the announcement grace is simply waited out — the waiter
    /// returns `Ok` and never announces, so a second process overlapping a window's
    /// seconds-long no-op tick puts nothing on the glass.
    #[test]
    fn lock_store_waiting_acquires_when_the_holder_drops_and_stays_silent_inside_the_grace() {
        let a = temp_layout("wait-drop");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(guard);
        });
        let started = Instant::now();
        let announced = std::cell::Cell::new(0u32);
        let got = lock_store_waiting(
            &b,
            Duration::from_secs(10),
            |_| announced.set(announced.get() + 1),
            || true,
        );
        assert!(
            got.is_ok(),
            "the waiter takes the lock once the holder drops"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "it actually waited: {:?}",
            started.elapsed()
        );
        assert_eq!(
            announced.get(),
            0,
            "a wait shorter than the grace announces nothing"
        );
        holder.join().unwrap();
        drop(got);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// Past the grace the wait is ANNOUNCED — exactly once, naming the lock path —
    /// and the waiter still proceeds when the holder finally lets go.
    #[test]
    fn lock_store_waiting_announces_once_after_the_grace_then_acquires() {
        let a = temp_layout("wait-announce");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let holder = std::thread::spawn(move || {
            std::thread::sleep(WAIT_ANNOUNCE_GRACE + Duration::from_millis(700));
            drop(guard);
        });
        let announced = std::cell::RefCell::new(Vec::<PathBuf>::new());
        let got = lock_store_waiting(
            &b,
            Duration::from_secs(20),
            |p| announced.borrow_mut().push(p.to_path_buf()),
            || true,
        );
        assert!(got.is_ok(), "acquired after the holder dropped");
        assert_eq!(
            announced.borrow().as_slice(),
            &[a.store_lock()],
            "announced once, with the lock path"
        );
        holder.join().unwrap();
        drop(got);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// A holder that never lets go: the waiter announces once, then times out with
    /// the SAME `Contended` refusal a typed verb gets — the sentence is unchanged
    /// (callers key on the exit code, never on the words).
    #[test]
    fn lock_store_waiting_times_out_with_contended() {
        let a = temp_layout("wait-timeout");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let _guard = try_lock_store(&a).expect("first acquisition succeeds");
        let timeout = WAIT_ANNOUNCE_GRACE + Duration::from_secs(1);
        let started = Instant::now();
        let announced = std::cell::Cell::new(0u32);
        let err = match lock_store_waiting(
            &b,
            timeout,
            |_| announced.set(announced.get() + 1),
            || true,
        ) {
            Ok(_) => panic!("a lock held for the whole wait must refuse"),
            Err(e) => e,
        };
        assert!(
            started.elapsed() >= timeout,
            "waited the whole bound: {:?}",
            started.elapsed()
        );
        assert!(
            matches!(err, StoreLockError::Contended(ref p) if *p == a.store_lock()),
            "the timeout is a contention refusal naming the path: {err:?}"
        );
        assert!(
            err.to_string()
                .contains("another atpkg process holds the store lock"),
            "the sentence is byte-stable: {err}"
        );
        assert_eq!(announced.get(), 1, "announced exactly once per wait");
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// An unusable prefix is not something to wait on: the `Io` refusal comes back
    /// at once, with no announcement.
    #[test]
    fn lock_store_waiting_never_waits_on_io() {
        let l = temp_layout("wait-badprefix");
        std::fs::write(&l.prefix, b"not a directory").unwrap();
        let started = Instant::now();
        let announced = std::cell::Cell::new(0u32);
        let err = match lock_store_waiting(
            &l,
            Duration::from_secs(5),
            |_| announced.set(announced.get() + 1),
            || true,
        ) {
            Ok(_) => panic!("a file-shaped prefix cannot yield a store lock"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreLockError::Io(..)), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "no wait on Io: {:?}",
            started.elapsed()
        );
        assert_eq!(announced.get(), 0);
        let _ = std::fs::remove_file(&l.prefix);
    }

    /// A waiter whose caller has gone (`still_wanted` says no — the window that
    /// spawned it quit) stands down with `Contended` instead of polling out its
    /// whole bound and then racing a successor's waiter for the freed lock.
    #[test]
    fn lock_store_waiting_stands_down_when_no_longer_wanted() {
        let a = temp_layout("wait-orphan");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let _guard = try_lock_store(&a).expect("first acquisition succeeds");
        let polls = std::cell::Cell::new(0u32);
        let started = Instant::now();
        let err = match lock_store_waiting(
            &b,
            Duration::from_secs(30),
            |_| panic!("stood down inside the grace: nothing to announce"),
            || {
                polls.set(polls.get() + 1);
                polls.get() < 2
            },
        ) {
            Ok(_) => panic!("held lock"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreLockError::Contended(..)), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stood down long before the bound: {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// A caller that goes away PAST the grace is still never announced to: the
    /// orphan check precedes the announcement on every poll, so the wait that
    /// would have printed its marker on this very poll stands down instead. (The
    /// window's waiter prints to its window's pipe; announcing to a dead window
    /// is EPIPE, not the silent stand-down the docs promise.)
    #[test]
    fn lock_store_waiting_never_announces_to_a_caller_that_has_gone() {
        let a = temp_layout("wait-orphan-late");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let _guard = try_lock_store(&a).expect("first acquisition succeeds");
        let started = Instant::now();
        // Wanted for slightly LESS than the grace, so the caller is gone by the
        // first poll at which the grace has elapsed — the ordering under test.
        let wanted_for = WAIT_ANNOUNCE_GRACE - Duration::from_millis(100);
        let announced = std::cell::Cell::new(0u32);
        let err = match lock_store_waiting(
            &b,
            Duration::from_secs(30),
            |_| announced.set(announced.get() + 1),
            || started.elapsed() < wanted_for,
        ) {
            Ok(_) => panic!("held lock"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreLockError::Contended(..)), "{err:?}");
        assert!(
            started.elapsed() >= wanted_for,
            "waited while wanted: {:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < WAIT_ANNOUNCE_GRACE + Duration::from_secs(2),
            "stood down at the first poll after the caller went: {:?}",
            started.elapsed()
        );
        assert_eq!(
            announced.get(),
            0,
            "a caller that has gone is never announced to"
        );
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// The contention code is `EX_TEMPFAIL` and shares nothing with the codes the CLI
    /// ACTUALLY exits with — read from the production source of the two files that
    /// build an `ExitCode` (`cli.rs`, `reroute.rs`), never from a hand-typed list
    /// that drifts: every literal `ExitCode::from(<n>)`, plus the named
    /// `reroute::REFUSAL_EXIT`, plus the literal returns of every `fn <verb>_code()
    /// -> u8` body that a verb RELAYS through `ExitCode::from(code)` (0.82.0's
    /// announcement ledger split `cmd_update_all` and `cmd_install_default_set` that
    /// way, so the pass's real verdict answers its own announcement); the one other
    /// non-literal argument is the lock edge's own match, which names this constant.
    /// A caller keying on 75 can therefore never mistake another refusal for
    /// contention.
    ///
    /// THE SPLIT IS EACH TEST MODULE'S OWN GATE, and a split that cannot find it
    /// panics. This scan used to cut `cli.rs` at the first `#[cfg(test)]`, which for
    /// a module gated `#[cfg(all(test, unix))]` was never the module: the first hit
    /// was inside the tests, so "production" quietly included them — and when the
    /// ledger put a real `#[cfg(test)] fn reset()` seam into production code
    /// (2026-09-11) the same split cut the scan to the file's first half, 47 of its
    /// 88 literal sites, with both relayed sites among the unscanned. A truncated
    /// scan passes for the wrong reason, which is the defect it exists to catch.
    #[test]
    fn contention_exit_code_is_temp_fail_and_unshared() {
        assert_eq!(CONTENDED_EXIT, 75);
        let mut in_use = std::collections::BTreeSet::new();
        in_use.insert(crate::reroute::REFUSAL_EXIT);
        let mut literal_sites = 0usize;
        let mut relayed = std::collections::BTreeSet::new();
        for (file, src, gate) in [
            ("cli.rs", include_str!("cli.rs"), "#[cfg(all(test, unix))]"),
            ("reroute.rs", include_str!("reroute.rs"), "#[cfg(test)]"),
        ] {
            let (production, _) = src
                .split_once(gate)
                .unwrap_or_else(|| panic!("{file}'s test module is gated `{gate}`"));
            assert!(
                !production.contains("mod tests {"),
                "{file}: the production half must hold none of the tests"
            );
            for (at, _) in production.match_indices("ExitCode::from(") {
                let arg = &production[at + "ExitCode::from(".len()..];
                let literal: String = arg.chars().take_while(|c| *c != ')').collect();
                if let Ok(code) = literal.trim().parse::<u8>() {
                    in_use.insert(code);
                    literal_sites += 1;
                } else if literal.trim() == "code" {
                    // `let code = <verb>_code();` … `ExitCode::from(code)`: follow
                    // the binding to the body and read its returns — every
                    // `return <n>;` and every block-final bare literal, each on a
                    // line of its own under rustfmt. A `return` of anything else
                    // is a shape this scan cannot read, and says so.
                    let bind = production[..at]
                        .rfind("let code = ")
                        .expect("a relayed exit code is bound as `let code = …`");
                    let callee: String = production[bind + "let code = ".len()..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    assert!(callee.ends_with("_code"), "{file}: relayed from {callee:?}");
                    let mut returns = 0usize;
                    for line in relayed_body(production, &callee)
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.starts_with("//"))
                    {
                        let code = if let Some(after) = line.strip_prefix("return ") {
                            after
                                .trim_end_matches(';')
                                .parse::<u8>()
                                .unwrap_or_else(|_| {
                                    panic!("{callee}: `{line}` is not a literal exit code")
                                })
                        } else if let Ok(bare) = line.parse::<u8>() {
                            bare
                        } else {
                            continue;
                        };
                        in_use.insert(code);
                        returns += 1;
                    }
                    assert!(returns >= 2, "{callee}: the relay scan read its returns");
                    relayed.insert(callee);
                } else {
                    let head: String = arg.chars().take(240).collect();
                    assert!(
                        head.contains("REFUSAL_EXIT") || head.contains("CONTENDED_EXIT"),
                        "an exit code neither literal nor a named constant: {head:?}"
                    );
                }
            }
        }
        // NON-VACUITY, spelled out: the `cli.rs` half reached past the seam that once
        // truncated it and holds the last verb; the scan found more than the
        // truncated half's 47 + 4 sites; both relayed verbs were followed.
        let cli_half = include_str!("cli.rs")
            .split_once("#[cfg(all(test, unix))]")
            .map(|(p, _)| p)
            .expect("split above");
        assert!(
            cli_half.contains("pub(crate) fn reset()") && cli_half.contains("fn cmd_seed("),
            "the ledger's `#[cfg(test)] fn reset()` seam and `cmd_seed` are inside the half"
        );
        assert!(
            literal_sites > 51 && [1u8, 2, 3, 127].iter().all(|c| in_use.contains(c)),
            "the scan found the CLI's real sites (non-vacuity): {in_use:?} from {literal_sites}"
        );
        assert_eq!(
            relayed.iter().map(String::as_str).collect::<Vec<_>>(),
            ["cmd_install_default_set_code", "cmd_update_all_code"],
            "the two announcement-answering verbs relay their body's code"
        );
        assert!(
            !in_use.contains(&CONTENDED_EXIT),
            "75 is reserved for contention; the CLI also exits with {in_use:?}"
        );
    }

    /// The body of `fn <callee>() -> u8 { … }` in `production`: from its signature to
    /// the next `}` at column 0 (rustfmt closes every top-level item that way).
    fn relayed_body<'a>(production: &'a str, callee: &str) -> &'a str {
        let sig = format!("fn {callee}() -> u8 {{");
        let start = production
            .find(&sig)
            .unwrap_or_else(|| panic!("{callee} is a top-level `fn … -> u8`"));
        let rest = &production[start + sig.len()..];
        let end = rest.find("\n}\n").expect("the body closes at column 0");
        &rest[..end]
    }
}
