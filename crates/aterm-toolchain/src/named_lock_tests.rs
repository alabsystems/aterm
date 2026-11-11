// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atc-named-{label}-"))
        .tempdir()
        .expect("a scratch dir")
}

fn holder(verb: &str) -> Holder {
    Holder::me(verb, Path::new("/checkouts/a"))
}

fn quiet(_: Option<&Holder>, _: Duration) {}

#[test]
fn a_record_reads_back_and_a_bare_pid_is_a_holder() {
    let h = Holder {
        pid: 42,
        since: 1_792_000_000,
        verb: "lab linux\tproof".into(),
        checkout: "/a b/c".into(),
        credit: Vec::new(),
        stalled: None,
    };
    let back = Holder::parse(&h.line()).expect("a record");
    assert_eq!(back.verb, "lab linux proof", "a tab in a field is a space");
    assert_eq!(
        (back.pid, back.since, back.checkout.as_str()),
        (42, h.since, "/a b/c")
    );
    assert_eq!(
        Holder::parse("4242\n").map(|h| (h.pid, h.since)),
        Some((4242, 0))
    );
    assert_eq!(Holder::parse(""), None);
    assert_eq!(Holder::parse("0\t1\tx\ty"), None, "pid 0 names nobody");
    assert!(
        h.describe(-7 * 3600).ends_with("since 10:46)"),
        "{}",
        h.describe(-7 * 3600)
    );
    assert_eq!(
        Holder::parse("4242").map(|h| h.describe(0)),
        Some("pid 4242".to_string())
    );
}

/// A record names the processes its holder is credited with on a second line, which reads
/// back only whole: a record cut before its newline, as one rewritten in place may be read,
/// credits nothing, and a record of one line, an older run's, credits nothing either.
#[test]
fn a_records_credit_reads_back_only_whole() {
    let h = Holder {
        credit: vec![501, 77_000],
        ..holder("lab linux exec")
    };
    let record = h.record();
    assert_eq!(record.lines().nth(1), Some("credit\t501\t77000"));
    assert_eq!(Holder::parse(&record), Some(h.clone()));
    let torn = record.trim_end_matches('\n');
    assert_eq!(
        Holder::parse(torn).map(|h| h.credit),
        Some(Vec::new()),
        "{torn:?}"
    );
    assert_eq!(Holder::parse(&h.line()).map(|h| h.credit), Some(Vec::new()));
    let many = Holder {
        credit: (1..2000).collect(),
        ..h
    };
    let record = many.record();
    assert!(
        record.len() < usize::try_from(RECORD).expect("fits"),
        "{}",
        record.len()
    );
    let back = Holder::parse(&record).expect("a record").credit;
    assert!(
        !back.is_empty() && many.credit.starts_with(&back),
        "{back:?}"
    );
}

#[test]
fn a_taken_lock_names_its_holder_until_it_goes() {
    let dir = scratch("names");
    let path = dir.path().join("lane.lock");
    let me = holder("lab linux proof");
    let held = take(&path, &me, Wait::No, &mut quiet, &|| false).expect("free");
    assert_eq!(held.waited(), Duration::ZERO, "a free lock is had at once");
    assert_eq!(
        held.holder(),
        &Holder {
            since: held.holder().since,
            ..me.clone()
        }
    );
    assert_eq!(read(&path).as_ref(), Some(held.holder()));
    match take(
        &path,
        &holder("lab linux up"),
        Wait::No,
        &mut quiet,
        &|| false,
    ) {
        Err(Refused::Held(Some(named))) => assert_eq!(&named, held.holder()),
        other => panic!("a held lock must name its holder: {other:?}"),
    }
    drop(held);
    assert_eq!(read(&path), None, "the record goes with the lock");
    drop(take(&path, &me, Wait::No, &mut quiet, &|| false).expect("free again"));
}

#[cfg(unix)]
#[test]
fn close_only_transfer_keeps_the_original_holder_record_and_native_lock() {
    use std::os::unix::fs::MetadataExt as _;

    let dir = scratch("close-only");
    let path = dir.path().join("lane.lock");
    let original = take(&path, &holder("original"), Wait::No, &mut quiet, &|| false)
        .expect("the original lock");
    let record = std::fs::read(&path).unwrap();
    let identity = original.metadata().unwrap();
    let transferred = original.into_close_only();
    let duplicate = transferred.try_clone().unwrap();
    let exported = duplicate.try_clone_fd().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), record);
    let actual = transferred.metadata().unwrap();
    assert_eq!(
        (actual.dev(), actual.ino()),
        (identity.dev(), identity.ino())
    );
    drop(transferred);
    assert!(matches!(
        take(&path, &holder("contender"), Wait::No, &mut quiet, &|| false),
        Err(Refused::Held(_))
    ));
    drop(duplicate);
    assert!(matches!(
        take(&path, &holder("contender"), Wait::No, &mut quiet, &|| false),
        Err(Refused::Held(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), record);
    drop(exported);
    let next = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the final transferred description to close",
        || match take(&path, &holder("next"), Wait::No, &mut quiet, &|| false) {
            Ok(next) => Some(next),
            Err(Refused::Held(_)) => None,
            Err(why) => panic!("reacquisition failed: {why:?}"),
        },
    );
    assert_eq!(read(&path).as_ref(), Some(next.holder()));
    assert_eq!(next.holder().verb, "next");
    drop(next);
    assert_eq!(read(&path), None, "ordinary record clearing remains intact");
}

/// The waiter names the holder once it has waited a tick, then has the lock as soon as the
/// holder lets go, and its record says since when it has it, not since it asked: the waiter
/// asks as of second 1, so a record that kept the asking time would say so. Negative control:
/// a waiter told not to wait answers Held at once.
#[test]
fn a_waiter_names_the_holder_and_takes_the_lock_when_it_goes() {
    let dir = scratch("waits");
    let path = dir.path().join("lane.lock");
    let first = holder("lab linux proof");
    let held = take(&path, &first, Wait::No, &mut quiet, &|| false).expect("free");
    let first = held.holder().clone();
    assert!(matches!(
        take(&path, &holder("w"), Wait::No, &mut quiet, &|| false),
        Err(Refused::Held(_))
    ));
    let (go, gone) = mpsc::channel::<()>();
    let letting_go = std::thread::spawn(move || {
        // The one-minute hang detector: the waiter says whom it waits for well before.
        gone.recv_timeout(Duration::from_secs(60))
            .expect("the waiter named its holder");
        let let_go = now_secs();
        drop(held);
        let_go
    });
    let mut heard = Vec::new();
    let started = Instant::now();
    let second = Holder {
        since: 1,
        ..holder("lab linux exec")
    };
    let taken = take(
        &path,
        &second,
        Wait::Block,
        &mut |h, _| {
            heard.push(h.cloned());
            let _ = go.send(());
        },
        &|| started.elapsed() > Duration::from_secs(60),
    )
    .expect("the lock, once its holder let go");
    let let_go = letting_go.join().expect("the holder's thread");
    assert_eq!(heard.first(), Some(&Some(first)), "{heard:?}");
    assert!(taken.waited() >= TICK, "{:?}", taken.waited());
    let record = read(&path).expect("the waiter's record");
    assert_eq!(&record, taken.holder());
    assert_eq!(
        (record.pid, record.verb.as_str(), record.checkout.as_str()),
        (second.pid, "lab linux exec", "/checkouts/a")
    );
    assert!(
        record.since >= let_go,
        "since {} is before the lock was let go at {let_go}",
        record.since
    );
    drop(taken);
}

/// A waiter whose caller says stop comes back without the lock, naming the holder.
#[test]
fn a_stopped_wait_names_the_holder() {
    let dir = scratch("stops");
    let path = dir.path().join("lane.lock");
    let held = take(
        &path,
        &holder("ship trust run"),
        Wait::No,
        &mut quiet,
        &|| false,
    )
    .expect("free");
    match take(&path, &holder("w"), Wait::Block, &mut quiet, &|| true) {
        Err(Refused::Stopped {
            holder: Some(named),
            signal: None,
        }) => assert_eq!(&named, held.holder()),
        other => panic!("a stopped wait must name its holder: {other:?}"),
    }
    drop(held);
}

/// Set, the test binary is [`standin_waits_for_the_lock`], on the lock at this path.
#[cfg(unix)]
const STANDIN: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_STANDIN";

/// Run by [`an_interrupt_ends_a_wait_for_a_held_lock`] in a process of its own: waits for the
/// lock [`STANDIN`] names, with SIGINT and SIGTERM at their default actions as a foreground
/// process has them (set here, not inherited from the runner), writing `waiting` beside it
/// once it waits, and exits 3, a tool's NOT RUN, naming what
/// ended the wait and the holder.
#[cfg(unix)]
#[test]
fn standin_waits_for_the_lock() {
    let Some(path) = std::env::var_os(STANDIN).map(std::path::PathBuf::from) else {
        return;
    };
    // SAFETY: SIG_DFL is a disposition, no handler of ours.
    unsafe {
        // SIGTERM at its default action too, whatever this test binary was started with: a
        // watch leaves an ignored one ignored, and the SIGTERM leg sends it.
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
    let waiting = path.with_file_name("waiting");
    let started = Instant::now();
    let got = take(
        &path,
        &holder("lab linux exec"),
        Wait::Block,
        &mut |_, _| {
            let _ = std::fs::write(&waiting, std::process::id().to_string());
        },
        &|| started.elapsed() > Duration::from_secs(60),
    );
    match got {
        Err(Refused::Stopped {
            holder: Some(h),
            signal: Some(sig),
        }) => {
            println!(
                "stopped by {} waiting for {}",
                crate::interrupt::name(sig),
                h.describe(0)
            );
            std::process::exit(3);
        }
        other => panic!("the wait ended otherwise: {other:?}"),
    }
}

/// A stand-in, killed when it is dropped still running (a test that failed before it
/// ended), so none outlives its test.
#[cfg(unix)]
struct Standin(aterm_uds::spawnfd::Child);

#[cfg(unix)]
impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// A process waiting for a held lock ends on SIGTERM, and on SIGINT where it is not
/// ignored, well inside the one-minute hang detector (the time is reported), naming the
/// holder and the signal, with nobody signalling the holder, which keeps the lock. Without
/// the wait's watch its caller's `stop` never says so, and the stand-in dies of the signal
/// instead (no exit 3).
#[cfg(unix)]
#[test]
fn an_interrupt_ends_a_wait_for_a_held_lock() {
    for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
        let dir = scratch("interrupt");
        let path = dir.path().join("lane.lock");
        let held = take(
            &path,
            &holder("lab linux proof"),
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        let mut child = Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "named_lock::tests::standin_waits_for_the_lock",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(STANDIN, &path)
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the stand-in"),
        );
        let waiting = dir.path().join("waiting");
        let pid: i32 = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the stand-in to wait for the lock",
            || std::fs::read_to_string(&waiting).ok()?.trim().parse().ok(),
        );
        let sent = Instant::now();
        // SAFETY: `kill` takes two integers; the pid is this test's own child.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
        let status = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the stand-in's end",
            || child.0.try_wait().ok().flatten(),
        );
        let took = sent.elapsed();
        let mut out = String::new();
        if let Some(mut o) = child.0.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut o, &mut out);
        }
        assert_eq!(status.code(), Some(3), "{word}: {status} {out}");
        assert!(
            out.contains(&format!(
                "stopped by {word} waiting for pid {}",
                held.holder().pid
            )),
            "{out}"
        );
        eprintln!("{word} ended the lock wait in {took:?}");
        assert_eq!(
            read(&path).as_ref(),
            Some(held.holder()),
            "the holder keeps it"
        );
        drop(held);
    }
}

/// Set, the test binary is [`standin_waits_in_the_chain`], on the lock at this path.
#[cfg(unix)]
const LINK: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_LINK";

/// Run by [`a_holder_waiting_in_a_chain_is_as_stuck_as_its_head`] as a link of a chain: waits
/// for the lock [`LINK`] names at a test's pace (its holder named every quarter of a second,
/// judged busy over a second, and stuck after one), writing
/// its pid to `waiting` beside it once it waits, for at most the hang detector.
#[cfg(unix)]
#[test]
fn standin_waits_in_the_chain() {
    let Some(path) = std::env::var_os(LINK).map(std::path::PathBuf::from) else {
        return;
    };
    let waiting = path.with_file_name("waiting");
    let started = Instant::now();
    let _ = take_paced(
        &path,
        &holder("lab linux exec"),
        Wait::Block,
        &mut |_, _| {
            let _ = std::fs::write(&waiting, std::process::id().to_string());
        },
        &|| started.elapsed() > aterm_spec::wait::MUST_HAPPEN,
        Pace {
            say_every: Duration::from_millis(250),
            stalled_after: Duration::from_secs(1),
        },
    );
}

/// Set, the test binary is [`standin_heads_the_chain`], its work going on while the file this
/// names is there.
#[cfg(unix)]
const HEAD: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_HEAD";

/// Set with [`HEAD`], the stand-in is the head's working child.
#[cfg(unix)]
const HEAD_WORKS: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_HEAD_WORKS";

/// The head [`standin_heads_the_chain`] is, or with [`HEAD_WORKS`] its working child.
#[cfg(unix)]
fn head(works: &Path, child: bool) -> Standin {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    if child {
        cmd.env(HEAD_WORKS, "1");
    }
    Standin(
        cmd.args([
            "--exact",
            "named_lock::tests::standin_heads_the_chain",
            "--test-threads=1",
        ])
        .env(HEAD, works)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the head"),
    )
}

/// Run by [`a_holder_waiting_in_a_chain_is_as_stuck_as_its_head`] as the head of a chain: its
/// child keeps a core busy while the file [`HEAD`] names is there and the head lives; once
/// that child ends, the head does nothing, for twice the hang detector at most.
#[cfg(unix)]
#[test]
fn standin_heads_the_chain() {
    let Some(works) = std::env::var_os(HEAD).map(std::path::PathBuf::from) else {
        return;
    };
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN * 2;
    if std::env::var_os(HEAD_WORKS).is_some() {
        let parent = std::os::unix::process::parent_id();
        let mut n = 0u64;
        while works.exists()
            && std::os::unix::process::parent_id() == parent
            && Instant::now() < until
        {
            for i in 0..1_000_000u64 {
                n = std::hint::black_box(n.wrapping_mul(31).wrapping_add(i));
            }
        }
        return;
    }
    let mut child = head(&works, true);
    let _ = child.0.wait();
    std::thread::sleep(until.saturating_duration_since(Instant::now()));
}

/// A HOLDER THAT WAITS IN ITS TURN IS AS STUCK AS WHAT IT WAITS BEHIND (CW-WAIT-UNBOUNDED): a
/// link waits, through the real wait and its mark in this user's waits, for a lock whose
/// record names a head whose child works while a file is there. While that child works, the
/// link's tree reads as waiting behind it and is never stuck, though it does nothing of its
/// own for thirty minutes of the stall's clock (its own CPU time, a lock wait's looks and its
/// sweeps, is reported and is no busy process's; the link names its holder every quarter of
/// a second where a wait names it every minute, so its reads are judged on the stall's
/// clock, each sweep standing for a minute's, as each read does); once the file goes, the
/// child ends, the head idles, and once the link's own waiter calls it stuck, the link's tree
/// reads stuck behind it for as long as the link's mark says. NEGATIVE CONTROL: the same reads
/// with the mark left out call the link stuck at the bound while its head still works.
#[cfg(unix)]
#[test]
fn a_holder_waiting_in_a_chain_is_as_stuck_as_its_head() {
    use crate::stall::{STALLED_AFTER, Stall, Waits, busy};
    if aterm_sysprobe::processes().is_none() || crate::waits::user_dir().is_none() {
        eprintln!("no process sweep or no home here: nothing to read");
        return;
    }
    let dir = scratch("chain");
    let path = dir.path().join("lane.lock");
    let works = dir.path().join("head-works");
    std::fs::write(&works, "").expect("the head's work");
    let head = head(&works, false);
    let held = take(
        &path,
        &Holder {
            pid: head.0.id(),
            ..holder("lab linux proof")
        },
        Wait::No,
        &mut quiet,
        &|| false,
    )
    .expect("free");
    let link = Standin(
        aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "named_lock::tests::standin_waits_in_the_chain",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(LINK, &path)
            .env(
                crate::waits::TEST_USER_VAR,
                crate::waits::user_dir().expect("a waits directory"),
            )
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the link"),
    );
    let pid: u32 = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the link to wait for the lock",
        || {
            std::fs::read_to_string(dir.path().join("waiting"))
                .ok()?
                .trim()
                .parse()
                .ok()
        },
    );
    let t0 = Instant::now();
    let minute = |m: u32| t0 + Duration::from_secs(60) * m;
    let (mut stall, mut unmarked) = (Stall::default(), Stall::default());
    let read = || Work {
        at: None,
        ..crate::stall::tree_work(pid, &[]).expect("the link is swept")
    };
    let first = read();
    let real = Instant::now();
    let mut last = first;
    for m in 0..=30 {
        last = read();
        assert_eq!(last.waits, Waits::Behind, "minute {m}: {last:?}");
        assert_eq!(stall.read(pid, Some(last), minute(m)), Some(Duration::ZERO));
        let alone = Work {
            waits: Waits::No,
            ..last
        };
        let span = unmarked.stuck(pid, Some(alone), minute(m), STALLED_AFTER);
        assert_eq!(span.is_some(), m >= 15, "minute {m} unmarked: {span:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (ran, over) = (last.cpu_ns.saturating_sub(first.cpu_ns), real.elapsed());
    eprintln!("the link's own CPU time while it waited: {ran} ns in {over:?}");
    assert!(
        !busy(ran, over),
        "a lock wait's looks read busy: {ran} ns in {over:?}"
    );
    std::fs::remove_file(&works).expect("the head idles");
    let (behind, span) = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the link's waiter to call its head stuck",
        || {
            let now = read();
            match now.waits {
                Waits::Stuck(span) => Some((now, span)),
                Waits::No | Waits::Behind => None,
            }
        },
    );
    assert!(span >= Duration::from_secs(1), "{span:?}");
    assert_eq!(stall.read(pid, Some(behind), minute(30)), Some(span));
    // The lock let go, the link has it and ends, its marks gone with its wait.
    drop(held);
    let mut link = link;
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the link's end", || {
        link.0.try_wait().ok().flatten()
    });
    assert!(
        !crate::waits::user_waiting().iter().any(|w| w.pid == pid),
        "the link's mark outlived its wait"
    );
    drop(head);
}

/// A blocked wait is marked for a ceiling to read, naming the holder, and the mark goes once
/// the waiter has the lock. Without the mark the look for it hangs past its detector.
#[test]
fn a_blocked_wait_is_marked_until_it_has_the_lock() {
    let dir = scratch("marked");
    let path = dir.path().join("lane.lock");
    let marks = dir.path().join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    let held = take(
        &path,
        &holder("lab linux proof"),
        Wait::No,
        &mut quiet,
        &|| false,
    )
    .expect("free");
    let pid = held.holder().pid;
    let waiter = {
        let (path, marks) = (path.clone(), marks.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            take_marked(
                &path,
                &holder("lab linux exec"),
                Wait::Block,
                &mut quiet,
                &|| started.elapsed() > Duration::from_secs(60),
                &|| vec![marks.clone()],
                Pace::default(),
                &crate::stall::tree_work,
            )
            .map(drop)
        })
    };
    let detector = Instant::now() + Duration::from_secs(60);
    let seen = loop {
        if let Some(w) = crate::waits::waiting(&marks).pop() {
            break w;
        }
        assert!(Instant::now() < detector, "the wait was never marked");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        seen.text.starts_with(&format!(
            "lab linux exec: waiting for pid {pid} (lab linux proof, /checkouts/a), which has held"
        )),
        "{}",
        seen.text
    );
    drop(held);
    assert!(waiter.join().expect("the waiter").is_ok());
    assert!(
        crate::waits::waiting(&marks).is_empty(),
        "the mark outlived the wait"
    );
}

/// A holder whose process tree does no work is called stuck once it has done none for the
/// wait's bound: the holder `say` hears names how long, and the mark says so for a ceiling,
/// naming its pid; the wait goes on, and the holder keeps its lock. Each read is handed the
/// processes the holder's record credits it with ([`NamedLock::credit`]), this process left
/// out. Negative control: a tree that runs a core between reads is never called stuck.
#[test]
fn a_holder_that_makes_no_progress_is_named_stuck() {
    use std::cell::RefCell;
    let dir = scratch("stuck");
    let path = dir.path().join("lane.lock");
    let marks = dir.path().join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    let mut held = take(
        &path,
        &holder("lab linux proof"),
        Wait::No,
        &mut quiet,
        &|| false,
    )
    .expect("free");
    let me = held.holder().pid;
    held.credit(&[77_000, me, 501, 77_000]).expect("credited");
    assert_eq!(read(&path).map(|h| h.credit), Some(vec![501, 77_000]));
    let pace = Pace {
        say_every: Duration::from_millis(100),
        stalled_after: Duration::from_millis(500),
    };
    let idle = Work {
        cpu_ns: 7,
        members: 1,
        ..Work::default()
    };
    for moves in [false, true] {
        let handed = RefCell::new(Vec::new());
        let ran = Instant::now();
        let work = |_: u32, credited: &[u32]| {
            handed.borrow_mut().push(credited.to_vec());
            Some(if moves {
                // A core since the wait began.
                let core = u64::try_from(ran.elapsed().as_nanos()).unwrap_or(u64::MAX);
                Work {
                    cpu_ns: core,
                    runnable_ns: core,
                    ..idle
                }
            } else {
                idle
            })
        };
        let lines = RefCell::new(Vec::new());
        let marked = RefCell::new(Vec::new());
        let started = Instant::now();
        let got = take_marked(
            &path,
            &holder("lab linux exec"),
            Wait::Block,
            &mut |h, _| {
                lines
                    .borrow_mut()
                    .push(h.map(|h| h.describe(0)).unwrap_or_default());
                marked
                    .borrow_mut()
                    .extend(crate::waits::waiting(&marks).pop());
            },
            // Fifteen lines, a tenth of a second apart: three times the bound.
            &|| lines.borrow().len() >= 15 || started.elapsed() > Duration::from_secs(60),
            &|| vec![marks.clone()],
            pace,
            &work,
        );
        assert!(matches!(got, Err(Refused::Stopped { .. })), "{got:?}");
        let handed = handed.into_inner();
        assert!(
            !handed.is_empty() && handed.iter().all(|c| c == &[501, 77_000]),
            "{handed:?}"
        );
        let (lines, marked) = (lines.into_inner(), marked.into_inner());
        let said = |l: &String| l.contains(", which has made no progress for ");
        let stuck: Vec<_> = marked.iter().filter_map(|w| w.stuck.clone()).collect();
        if moves {
            assert!(!lines.iter().any(said), "{lines:#?}");
            assert!(stuck.is_empty(), "{marked:#?}");
        } else {
            assert!(!said(&lines[0]), "stuck at its first read: {lines:#?}");
            assert!(lines.last().is_some_and(said), "{lines:#?}");
            let last = stuck.last().expect("the mark said it was stuck");
            assert_eq!(last.holders, [held.holder().pid]);
            assert!(last.span >= pace.stalled_after, "{last:?}");
            assert!(
                marked
                    .last()
                    .is_some_and(|w| w.text.contains(", and which has made no progress")),
                "{marked:#?}"
            );
        }
        assert_eq!(
            read(&path).as_ref(),
            Some(held.holder()),
            "the holder keeps it"
        );
    }
}

#[test]
fn a_dead_pids_record_is_not_its_holder() {
    let h = holder("lab linux proof");
    let live = name(Some(&h), |_| 0, |_| true);
    assert!(live.starts_with(&format!("pid {}", h.pid)), "{live}");
    let stale = name(Some(&h), |_| 0, |_| false);
    assert!(stale.starts_with("a run that wrote no record"), "{stale}");
    assert!(stale.contains(&format!("pid {}", h.pid)), "{stale}");
    assert!(!name(None, |_| 0, |_| true).contains("pid"));
    #[cfg(unix)]
    {
        let dir = scratch("nofollow");
        let real = dir.path().join("real.lock");
        std::fs::write(&real, h.line()).unwrap();
        let link = dir.path().join("link.lock");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(read(&link), None, "read must not follow a symlink");
        assert!(
            matches!(
                take(&link, &h, Wait::No, &mut quiet, &|| false),
                Err(Refused::Io(_))
            ),
            "take must refuse a symlinked lock path"
        );
        let fifo = dir.path().join("fifo.lock");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(read(&fifo), None, "read must not block on a FIFO");
        assert!(
            matches!(
                take(&fifo, &h, Wait::No, &mut quiet, &|| false),
                Err(Refused::Io(_))
            ),
            "take must refuse a FIFO lock path"
        );
    }
}
