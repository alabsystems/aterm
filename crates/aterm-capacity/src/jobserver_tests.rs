// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::wait::MUST_HAPPEN;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

/// Marks the run of [`adopting_child_actor`] its parent test started.
const CHILD_ACTOR: &str = "ATERM_JOBSERVER_TEST_CHILD";

fn every_free_token(pool: &Jobserver) -> Vec<Token<'_>> {
    std::iter::from_fn(|| pool.try_acquire().unwrap()).collect()
}

#[test]
fn branches_and_their_workers_share_exactly_the_grant() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 3).unwrap();
    let mut native = aterm_uds::spawnfd::command("targo");
    let mut compat = aterm_uds::spawnfd::command("cargo");
    let started = std::time::Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    let first = pool.attach_cargo(&mut native, &stopped).unwrap();
    let second = pool.attach_cargo(&mut compat, &stopped).unwrap();
    for cmd in [&native, &compat] {
        for name in ["MAKEFLAGS", "CARGO_MAKEFLAGS"] {
            assert_eq!(
                cmd.get_envs()
                    .find(|(k, _)| *k == name)
                    .and_then(|(_, v)| v),
                Some(std::ffi::OsStr::new(&pool.flags()))
            );
        }
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["-j", "3"]);
    }
    // A second FIFO opener represents another process, not a private pool per branch.
    let worker = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    let mut byte = [0];
    (&worker).read_exact(&mut byte).unwrap();
    assert!(
        pool.try_acquire().unwrap().is_none(),
        "three clients exhaust S=3"
    );
    drop(first);
    let returned = pool
        .try_acquire()
        .unwrap()
        .expect("implicit token returned");
    assert!(
        pool.try_acquire().unwrap().is_none(),
        "negative control: no extra implicit token"
    );
    drop((second, returned));
    (&worker).write_all(&byte).unwrap();
    let all: Vec<_> = (0..3)
        .map(|_| pool.try_acquire().unwrap().unwrap())
        .collect();
    assert!(pool.try_acquire().unwrap().is_none());
    assert!(
        Jobserver::new(&path, 3).is_err(),
        "never replace a live pool"
    );
    drop(all);
    drop(pool);
    assert!(!path.exists());
}

#[test]
fn an_adopted_pool_shares_the_creators_tokens_and_leaves_its_fifo_to_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 3).unwrap();
    let mut handed = OsString::from("3:");
    handed.push(pool.path());
    assert_eq!(pool.handed(), handed);
    assert!(pool.path().is_absolute());
    assert_eq!(
        pool.path(),
        path,
        "the FIFO made, whatever TMPDIR's spelling"
    );
    let mine = pool.try_acquire().unwrap().unwrap();
    let adopted = Jobserver::adopt(&pool.handed()).unwrap();
    assert_eq!(adopted.slots(), 3);
    assert_eq!(adopted.flags(), pool.flags());
    let theirs = every_free_token(&adopted);
    assert_eq!(
        theirs.len(),
        2,
        "the adopter takes what the creator left free, no more"
    );
    assert!(pool.try_acquire().unwrap().is_none());
    drop(theirs);
    drop(adopted);
    assert!(
        path.exists(),
        "an adopted handle never removes its creator's FIFO"
    );
    assert_eq!(
        every_free_token(&pool).len(),
        2,
        "the adopter's tokens came back"
    );
    drop(mine);
    drop(pool);
    assert!(!path.exists(), "the creator removes it");
}

/// A share of the pool is handed as [`POOL_ENV`]'s `<slots>:<fifo>` with the share's count and
/// the very path [`Jobserver::hand_to`] hands, whatever spelling the pool was made by, and an
/// adopter reads the share as its slots on the same FIFO; a share of every slot is the whole
/// pool, and a share of none or of more than the pool is refused and hands nothing. NEGATIVE
/// CONTROLS: handing the pool's own count in place of the share, or dropping the bound, fails it.
#[test]
fn a_share_of_the_pool_is_the_same_fifo_with_fewer_slots() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join(".").join("jobs"), 4).unwrap();
    let handed = |cmd: &aterm_uds::spawnfd::Command| {
        let value = cmd
            .get_envs()
            .find(|(k, _)| *k == POOL_ENV)
            .and_then(|(_, v)| v)?;
        let (slots, fifo) = value.to_str()?.split_once(':')?;
        Some((slots.to_owned(), OsString::from(fifo)))
    };
    let mut whole = aterm_uds::spawnfd::command("ship");
    pool.hand_to(&mut whole);
    let mut part = aterm_uds::spawnfd::command("targo");
    pool.hand_share_to(&mut part, 3).unwrap();
    let fifo = pool.path().as_os_str().to_owned();
    assert_eq!(pool.path(), dir.path().join("jobs"), "the FIFO asked for");
    assert_eq!(handed(&whole), Some(("4".to_owned(), fifo.clone())));
    assert_eq!(handed(&part), Some(("3".to_owned(), fifo)));
    assert_eq!(pool.handed_share(4).unwrap(), pool.handed());
    let adopted = Jobserver::adopt(&pool.handed_share(3).unwrap()).unwrap();
    assert_eq!(adopted.slots(), 3);
    assert_eq!(adopted.path(), pool.path());
    assert_eq!(adopted.flags(), pool.flags(), "the same FIFO");
    for slots in [0, 5] {
        assert!(pool.handed_share(slots).is_err(), "a share of {slots}");
        let mut refused = aterm_uds::spawnfd::command("targo");
        assert!(pool.hand_share_to(&mut refused, slots).is_err());
        assert!(
            refused.get_envs().all(|(k, _)| k != POOL_ENV),
            "a refused share hands nothing"
        );
    }
}

/// The child half of [`a_child_process_adopts_the_pool_and_never_holds_more_than_is_free`]: with
/// its parent's marker, it adopts the pool it was handed and takes every token free. Without
/// the marker it does nothing.
#[test]
fn adopting_child_actor() {
    if std::env::var_os(CHILD_ACTOR).is_none() {
        return;
    }
    let pool = Jobserver::inherited()
        .expect("the parent handed a pool")
        .unwrap();
    let held = every_free_token(&pool);
    println!("took={}", held.len());
}

#[test]
fn a_child_process_adopts_the_pool_and_never_holds_more_than_is_free() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 3).unwrap();
    let mine = pool.try_acquire().unwrap().unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "jobserver::tests::adopting_child_actor",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_ACTOR, "1")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::inherit());
    pool.hand_to(&mut child);
    assert_eq!(
        child
            .get_envs()
            .find(|(k, _)| *k == POOL_ENV)
            .and_then(|(_, v)| v),
        Some(pool.handed().as_os_str())
    );
    assert!(
        child
            .get_envs()
            .all(|(k, _)| k != "MAKEFLAGS" && k != "CARGO_MAKEFLAGS"),
        "a branch is handed the pool, not made one GNU client"
    );
    let out = child.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let said = String::from_utf8_lossy(&out.stdout);
    // libtest prints the test's name before its output, on the same line.
    let took: Vec<&str> = said
        .lines()
        .filter_map(|l| Some(l.split_once("took=")?.1.trim()))
        .collect();
    assert_eq!(took, ["2"], "parent holds 1 of S=3: {said}");
    assert!(path.exists(), "the child's handle left the FIFO");
    assert_eq!(
        every_free_token(&pool).len(),
        2,
        "the child returned what it took"
    );
    drop(mine);
}

#[test]
fn malformed_or_foreign_pool_values_are_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 2).unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, b"++").unwrap();
    let fifo = path.display().to_string();
    for value in [
        String::new(),
        "2".into(),
        format!(":{fifo}"),
        format!("0:{fifo}"),
        format!("4097:{fifo}"),
        format!("+2:{fifo}"),
        format!(" 2:{fifo}"),
        format!("2x:{fifo}"),
        "2:relative/jobs".into(),
        format!("2:{}", dir.path().join("missing").display()),
        format!("2:{}", dir.path().join("with space").display()),
        format!("2:{}", plain.display()),
    ] {
        assert!(Jobserver::adopt(OsStr::new(&value)).is_err(), "{value:?}");
    }
    let adopted = Jobserver::adopt(OsStr::new(&format!("2:{fifo}"))).unwrap();
    assert_eq!(adopted.slots(), 2);
    drop(adopted);
    drop(pool);
}

#[test]
fn a_waiter_is_woken_by_the_write_not_by_its_clock() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 1).unwrap();
    // The one token, held by another holder of the pool, which returns it by a write to the
    // FIFO (a token of this handle would be handed to the waiter instead, never written).
    let mut byte = [0];
    (&pool.file).read_exact(&mut byte).unwrap();
    let started = Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    let (waiting, is_waiting) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            // Its clock would wake it only at the hang detector: a token before then came from
            // the write.
            pool.acquire_waking(&stopped, MUST_HAPPEN, &|| waiting.send(()).unwrap())
                .map(drop)
        });
        // The waiter has found the FIFO empty and is about to wait: the token comes back now.
        is_waiting.recv().unwrap();
        (&pool.file).write_all(&byte).unwrap();
        // Negative control: a waiter its clock woke would end at its stop predicate, an error.
        waiter.join().unwrap().unwrap();
    });
    assert!(pool.try_acquire().unwrap().is_some(), "the token is back");
}

/// A wait whose caller stops ends its reader's blocked `read(2)` and leaves nothing reading the
/// FIFO: it ends with the stop's error well before the hang detector, and the token returned
/// after it is in the FIFO, not taken by a reader the stop left behind. NEGATIVE CONTROL: a stop
/// that only stops asking, leaving the reader in its read, never ends the wait (the test then
/// frees the reader with the token, which it takes: no error, and nothing in the FIFO).
#[test]
fn a_stopped_wait_ends_its_read_and_leaves_no_reader_behind() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 1).unwrap();
    let mut byte = [0];
    (&pool.file).read_exact(&mut byte).unwrap();
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    let stopped = || stop.load(Ordering::SeqCst) || started.elapsed() >= MUST_HAPPEN;
    let (waiting, is_waiting) = mpsc::channel();
    let (ended, has_ended) = mpsc::channel();
    let outcome = std::thread::scope(|scope| {
        scope.spawn(|| {
            let got = pool.acquire_waking(&stopped, WAKE_EVERY, &|| {
                let _ = waiting.send(());
            });
            ended.send(got.map(drop)).unwrap();
        });
        is_waiting.recv().unwrap();
        stop.store(true, Ordering::SeqCst);
        let outcome = has_ended.recv_timeout(MUST_HAPPEN);
        // Whatever the wait did, the token comes back now, so a reader it left can end.
        (&pool.file).write_all(&byte).unwrap();
        outcome.unwrap_or_else(|_| has_ended.recv().unwrap())
    });
    let error = outcome.expect_err("the stop ends the wait with no token");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(
        started.elapsed() < MUST_HAPPEN,
        "the stop, not the hang detector, ended the wait"
    );
    assert!(
        pool.try_acquire().unwrap().is_some(),
        "the token returned is in the FIFO: no reader of the stopped wait took it"
    );
}

/// A token that reaches a wait after its caller stopped, before the wait would ask `stopped`
/// again, goes back. A write may bring it to the wait's reader, or this handle may hand the
/// wait its own return. Either way the wait ends with the stop's error and the token is in the
/// FIFO, so a token comes back only after `stopped` said no. NEGATIVE CONTROL: returning the
/// wait's token without asking `stopped` again ends each wait with that token, after the stop.
#[test]
fn a_token_that_reaches_a_wait_after_its_stop_goes_back() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 1).unwrap();
    for handed in [false, true] {
        // The one token: this handle's, whose return is handed to the wait, or another
        // holder's, whose return is a write to the FIFO.
        let (mut held, mut byte) = (None, [0]);
        if handed {
            held = pool.try_acquire().unwrap();
        } else {
            (&pool.file).read_exact(&mut byte).unwrap();
        }
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let stopped = || stop.load(Ordering::SeqCst) || started.elapsed() >= MUST_HAPPEN;
        let (waiting, is_waiting) = mpsc::channel();
        let outcome = std::thread::scope(|scope| {
            // Its clock would ask `stopped` only at the hang detector, so the stop is seen
            // only where a token ends the wait.
            let waiter = scope.spawn(|| {
                pool.acquire_waking(&stopped, MUST_HAPPEN, &|| {
                    let _ = waiting.send(());
                })
                .map(drop)
            });
            is_waiting.recv().unwrap();
            // The stop comes first; then the token reaches the waiting acquire.
            stop.store(true, Ordering::SeqCst);
            match held.take() {
                Some(token) => drop(token),
                None => (&pool.file).write_all(&byte).unwrap(),
            }
            waiter.join().unwrap()
        });
        let error = outcome.expect_err("a token after the stop is no token for the caller");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "handed: {handed}");
        assert!(
            started.elapsed() < MUST_HAPPEN,
            "the token, not the hang detector, ended the wait (handed: {handed})"
        );
        assert_eq!(
            every_free_token(&pool).len(),
            1,
            "the token went back to the FIFO (handed: {handed})"
        );
    }
}

/// Block [`INTERRUPT`] in the calling thread's signal mask. A caller may do this itself, and a
/// process can start with it blocked, because a mask survives `fork` and `exec`.
fn block_interrupt_in_this_thread() {
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: sigemptyset initialises the set it is handed and sigaddset adds a valid signal to
    // it; pthread_sigmask changes only this thread's mask, from that initialised set.
    let blocked = unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        libc::sigaddset(set.as_mut_ptr(), INTERRUPT);
        libc::pthread_sigmask(libc::SIG_BLOCK, set.as_ptr(), std::ptr::null_mut())
    };
    assert_eq!(blocked, 0, "block SIGURG in the waiter's thread");
}

/// A wait still ends its read when it must, though its caller's thread blocks [`INTERRUPT`]: its
/// reader starts with that mask and unblocks the signal in its own. A stopped wait ends with the
/// stop's error, and a wait handed this handle's return ends with that token. Each ends before
/// the hang detector, and every token is back afterwards. NEGATIVE CONTROL: a reader that keeps
/// the mask it started with leaves each signal pending and its read blocked. Each wait then ends
/// only at the hang detector, when the test frees that reader with another holder's token.
#[test]
fn a_wait_ends_its_read_though_its_caller_blocks_the_interrupt() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 2).unwrap();
    for handed in [false, true] {
        // Another holder's token, which frees a reader the wait left in its read, and this
        // handle's, which a handed wait gets: the FIFO is empty.
        let mut byte = [0];
        (&pool.file).read_exact(&mut byte).unwrap();
        let mut held = pool.try_acquire().unwrap();
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let stopped = || stop.load(Ordering::SeqCst) || started.elapsed() >= MUST_HAPPEN;
        let (waiting, is_waiting) = mpsc::channel();
        let (ended, has_ended) = mpsc::channel();
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                block_interrupt_in_this_thread();
                let got = pool.acquire_waking(&stopped, WAKE_EVERY, &|| {
                    let _ = waiting.send(());
                });
                ended.send(got.map(drop)).unwrap();
            });
            is_waiting.recv().unwrap();
            if handed {
                drop(held.take());
            } else {
                stop.store(true, Ordering::SeqCst);
            }
            let outcome = has_ended.recv_timeout(MUST_HAPPEN);
            // Whatever the wait did, the other holder's token comes back now, so a reader it
            // left can end.
            (&pool.file).write_all(&byte).unwrap();
            outcome.unwrap_or_else(|_| has_ended.recv().unwrap())
        });
        assert!(
            started.elapsed() < MUST_HAPPEN,
            "the {} ended the wait's read, not the hang detector",
            if handed { "handed token" } else { "stop" }
        );
        if handed {
            outcome.expect("the handed token ends the wait with it");
        } else {
            let error = outcome.expect_err("the stop ends the wait with no token");
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        }
        drop(held);
        assert_eq!(
            every_free_token(&pool).len(),
            2,
            "none lost, none made (handed: {handed})"
        );
    }
}

/// A stop predicate that panics mid-wait ends the wait as a stop does, then the panic goes on:
/// the waiter's thread panics before the hang detector, and the wait left neither a reader in
/// the FIFO nor a record a return would be handed to. NEGATIVE CONTROL: a panic let through the
/// wait's scope leaves it joining a reader still blocked in `read(2)`, so the wait ends only
/// once the test frees that reader with the token, which its record then keeps.
#[test]
fn a_panicking_stop_predicate_ends_the_wait_and_keeps_every_token() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 1).unwrap();
    let mut byte = [0];
    (&pool.file).read_exact(&mut byte).unwrap();
    let started = Instant::now();
    let asked = std::sync::atomic::AtomicU32::new(0);
    // Asked first before the wait, then each `wake` inside it, where it panics.
    let stopped = || {
        assert!(
            asked.fetch_add(1, Ordering::SeqCst) == 0,
            "a stop predicate's panic mid-wait"
        );
        false
    };
    let (ended, has_ended) = mpsc::channel();
    let outcome = std::thread::scope(|scope| {
        scope.spawn(|| {
            let waiter = std::thread::scope(|inner| {
                inner
                    .spawn(|| pool.acquire_waking(&stopped, WAKE_EVERY, &|| {}).map(drop))
                    .join()
            });
            ended.send(waiter.is_err()).unwrap();
        });
        let outcome = has_ended.recv_timeout(MUST_HAPPEN);
        // Whatever the wait did, the token comes back now, so a reader it left can end.
        (&pool.file).write_all(&byte).unwrap();
        outcome.unwrap_or_else(|_| has_ended.recv().unwrap())
    });
    assert!(outcome, "the predicate's panic reached the waiter's caller");
    assert!(
        started.elapsed() < MUST_HAPPEN,
        "the panic, not the hang detector, ended the wait"
    );
    let token = pool
        .try_acquire()
        .unwrap()
        .expect("the token returned is in the FIFO: no reader of the wait took it");
    drop(token);
    assert!(
        pool.try_acquire().unwrap().is_some(),
        "a return goes to the FIFO: no record of the ended wait was handed it"
    );
}

/// A token this handle returns while one of its waits waits is handed to that wait, never
/// written to the FIFO, though a blocked reader that has slept longer waits there for it (as
/// Cargo keeps a finished job's token for its next queued job). NEGATIVE CONTROL: writing the
/// return to the FIFO lets the helper, which has slept longer, take it in most rounds.
#[test]
fn a_token_the_handle_returns_goes_to_its_waiting_acquire_not_the_fifo() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 1).unwrap();
    let mut held = pool.try_acquire().unwrap();
    // Cargo's helper thread: its own descriptor, blocking, in read(2) whenever it has no token.
    let helper = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let done = AtomicBool::new(false);
    let (took, taken) = mpsc::channel();
    let started = Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    std::thread::scope(|scope| {
        let (helper, done) = (&helper, &done);
        scope.spawn(move || {
            let mut helper: &File = helper;
            loop {
                let mut b = [0];
                helper.read_exact(&mut b).unwrap();
                if !done.load(Ordering::SeqCst) {
                    took.send(()).unwrap();
                }
                // Its job ran on it; the token goes back, so the round goes on.
                helper.write_all(&b).unwrap();
                if done.load(Ordering::SeqCst) {
                    return;
                }
            }
        });
        for _ in 0..20 {
            let (waiting, is_waiting) = mpsc::channel();
            let waiter = scope.spawn(|| {
                pool.acquire_waking(&stopped, WAKE_EVERY, &move || {
                    let _ = waiting.send(());
                })
            });
            is_waiting.recv().unwrap();
            drop(held.take());
            held = Some(waiter.join().unwrap().unwrap());
        }
        done.store(true, Ordering::SeqCst);
        drop(held.take());
    });
    assert_eq!(
        taken.try_iter().count(),
        0,
        "the helper took none of the handle's returns"
    );
    assert!(
        pool.try_acquire().unwrap().is_some(),
        "the one token is back"
    );
}

/// However this handle's waits, its returns and a blocked reader's reads interleave (a token
/// handed to a wait whose reader took one too, a wait whose reader the handoff ends), the pool
/// keeps every token: none lost, none made. NEGATIVE CONTROL: dropping the byte a reader took
/// beside a handed one, in place of giving it on, ends with fewer tokens than slots.
#[test]
fn waits_returns_and_a_blocked_reader_keep_every_token() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 3).unwrap();
    let helper = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let done = AtomicBool::new(false);
    let started = Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    std::thread::scope(|scope| {
        let (helper, done) = (&helper, &done);
        scope.spawn(move || {
            let mut helper: &File = helper;
            loop {
                let mut b = [0];
                helper.read_exact(&mut b).unwrap();
                helper.write_all(&b).unwrap();
                if done.load(Ordering::SeqCst) {
                    return;
                }
            }
        });
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    for _ in 0..200 {
                        drop(pool.acquire(&stopped).unwrap());
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        done.store(true, Ordering::SeqCst);
        // The helper is in read(2) or about to be: one of the pool's tokens ends it.
        drop(pool.acquire(&stopped).unwrap());
    });
    assert_eq!(every_free_token(&pool).len(), 3, "none lost, none made");
}

/// What the threads of [`a_waiter_gets_a_token_while_a_blocked_reader_competes_for_it`] tell the
/// test.
enum Said {
    /// Cargo's helper took the token and its job ran on it: the test returns it.
    Byte(u8),
    /// The waiter is about to wait: as it begins its wait, and again each `wake` it waits on.
    Waiting,
    /// The waiter holds the token.
    Got,
    Failed(String),
}

#[test]
fn a_waiter_gets_a_token_while_a_blocked_reader_competes_for_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs");
    let pool = Jobserver::new(&path, 1).unwrap();
    // The one token, as a byte the test returns by hand.
    let mut byte = [0];
    (&pool.file).read_exact(&mut byte).unwrap();
    // Cargo's helper thread: its own descriptor, blocking, in read(2) whenever it has no token.
    let helper = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let done = AtomicBool::new(false);
    let (said, heard) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let started = Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    let (pool, helper, done, stopped) = (&pool, &helper, &done, &stopped);
    std::thread::scope(|scope| {
        let to_test = said.clone();
        scope.spawn(move || {
            let mut helper: &File = helper;
            loop {
                let mut b = [0];
                helper.read_exact(&mut b).unwrap();
                if done.load(Ordering::SeqCst) {
                    helper.write_all(&b).unwrap();
                    return;
                }
                to_test.send(Said::Byte(b[0])).unwrap();
            }
        });
        let to_test = said.clone();
        scope.spawn(move || {
            match pool.acquire_waking(stopped, WAKE_EVERY, &|| {
                to_test.send(Said::Waiting).unwrap();
            }) {
                Ok(token) => {
                    to_test.send(Said::Got).unwrap();
                    // The test lets the token go once it has told the helper to stop.
                    let _released = released.recv();
                    drop(token);
                }
                Err(why) => to_test.send(Said::Failed(why.to_string())).unwrap(),
            }
        });
        // The test returns a token only once the waiter waits, as another holder of the pool
        // would, by a write to the FIFO: each write but the wait's first lands with the waiter's
        // reader in read(2) beside the helper in read(2), and only the race decides who takes it
        // (the first can land before the waiter's reader has started, and so goes to the helper).
        let (mut held, mut armed, mut races) = (Some(byte[0]), false, 0u64);
        loop {
            match heard.recv().unwrap() {
                Said::Byte(b) => held = Some(b),
                Said::Waiting => armed = true,
                Said::Got => break,
                Said::Failed(why) => {
                    done.store(true, Ordering::SeqCst);
                    (&pool.file).write_all(&[held.unwrap_or(b'+')]).unwrap();
                    panic!(
                        "the waiter never won a token in {races} races with a blocked reader: {why}"
                    );
                }
            }
            if armed && let Some(b) = held.take() {
                armed = false;
                races += 1;
                (&pool.file).write_all(&[b]).unwrap();
            }
        }
        done.store(true, Ordering::SeqCst);
        release.send(()).unwrap();
    });
    assert!(
        pool.try_acquire().unwrap().is_some(),
        "the one token is back"
    );
}

#[test]
fn partial_charges_on_one_handle_take_turns_instead_of_splitting_the_pool() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 2).unwrap();
    let started = Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    // Two binaries each charged the whole pool, the pool held elsewhere until both wait: split
    // one token each, they would wait on each other until the hang detector.
    for _ in 0..20 {
        let held = every_free_token(&pool);
        assert_eq!(held.len(), 2);
        let ready = std::sync::Barrier::new(3);
        let (said, heard) = mpsc::channel();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let said = said.clone();
                let (ready, stopped) = (&ready, &stopped);
                let pool = &pool;
                scope.spawn(move || {
                    ready.wait();
                    let charge = pool.acquire_many(2, stopped).map(|c| c.len());
                    said.send(charge.map_err(|e| e.to_string())).unwrap();
                });
            }
            ready.wait();
            drop(held);
            for _ in 0..2 {
                assert_eq!(heard.recv().unwrap(), Ok(2));
            }
        });
    }
    assert_eq!(every_free_token(&pool).len(), 2);
    assert_eq!(
        pool.acquire_many(5, &stopped).unwrap().len(),
        2,
        "a charge is at most the pool"
    );
    assert!(pool.acquire_many(0, &stopped).unwrap().is_empty());
}

#[test]
fn a_stopped_partial_charge_returns_what_it_took() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 3).unwrap();
    let mine = pool.try_acquire().unwrap().unwrap();
    let asked = std::sync::atomic::AtomicU32::new(0);
    // Two tokens free of the three it asks: it takes them, then stops waiting for the third.
    let stopped = || asked.fetch_add(1, Ordering::SeqCst) >= 3;
    let error = pool.acquire_many(3, &stopped).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(every_free_token(&pool).len(), 2, "nothing kept on a stop");
    drop(mine);
}
