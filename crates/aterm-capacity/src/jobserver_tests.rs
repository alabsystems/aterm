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
    let held = pool.try_acquire().unwrap().unwrap();
    let started = Instant::now();
    let stopped = || started.elapsed() >= MUST_HAPPEN;
    let (waiting, is_waiting) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            // Its clock would wake it only at the hang detector: a token before then came from
            // the write that woke it.
            pool.acquire_waking(&stopped, MUST_HAPPEN, &|| waiting.send(()).unwrap())
                .map(drop)
        });
        // The waiter has found the FIFO empty and is about to wait: the token comes back now.
        is_waiting.recv().unwrap();
        drop(held);
        // Negative control: a waiter its clock woke would end at its stop predicate, an error.
        waiter.join().unwrap().unwrap();
    });
    assert!(pool.try_acquire().unwrap().is_some(), "the token is back");
}

#[test]
fn a_poll_reads_nothing_and_ends_on_a_write() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = Jobserver::new(&dir.path().join("jobs"), 1).unwrap();
    let held = pool.try_acquire().unwrap().unwrap();
    std::thread::scope(|scope| {
        let poll = scope.spawn(|| pool.readable_within(MUST_HAPPEN).unwrap());
        drop(held);
        assert!(poll.join().unwrap(), "readable before the hang detector");
    });
    assert!(
        pool.try_acquire().unwrap().is_some(),
        "the wait left the byte for its reader"
    );
}

/// What the threads of [`a_waiter_gets_a_token_while_a_blocked_reader_competes_for_it`] tell the
/// test.
enum Said {
    /// Cargo's helper took the token and its job ran on it: the test returns it.
    Byte(u8),
    /// The waiter found the FIFO empty and is about to wait.
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
        // The test returns a token only once the waiter waits: each write wakes the waiter in
        // poll(2) beside the helper in read(2), and only the race decides who takes it.
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
