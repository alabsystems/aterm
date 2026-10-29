// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-slot record parse, two-read judgement, orphan rule, and live flock tests.

use std::path::PathBuf;

use super::*;

/// A fixed epoch instant for the pure tables.
const NOW: u64 = 1_800_000_000_000_000;
/// A machine whose 5-minute load no slot count explains.
const BUSY: Machine = Machine {
    load5: Some(1e4),
    ncpu: Some(18),
};
/// A quiet machine.
const CALM: Machine = Machine {
    load5: Some(0.0),
    ncpu: Some(18),
};

fn rec(pid: u32, started_us: u64) -> Record {
    Record {
        pid,
        started_us,
        cwd: Some(PathBuf::from("/w")),
    }
}

fn holder(pid: u32) -> Holder {
    Holder {
        pid,
        birth_us: NOW - 5_000_000,
    }
}

fn gone(pid: u32, started_us: u64) -> Sight {
    Sight::NotHeld {
        gone: Some(rec(pid, started_us)),
    }
}

fn obs(mono_ms: u64, sights: &[Sight]) -> Observation {
    Observation {
        mono_ms,
        now_us: NOW,
        slots: sights
            .iter()
            .zip(0u32..)
            .map(|(sight, index)| SlotObs {
                index,
                file: (1, 100 + u64::from(index)),
                sight: sight.clone(),
            })
            .collect(),
    }
}

fn counts_of(read: &SlotsRead) -> SlotCounts {
    match read {
        SlotsRead::Read { counts, .. } => *counts,
        SlotsRead::Unread(w) => panic!("a reading, not Unread({w:?})"),
    }
}

fn never(_: &Record) -> bool {
    false
}

#[test]
fn a_record_parses_as_python_writes_it_and_nothing_else() {
    let five = |started_us, cwd: Option<&str>| {
        RecordRead::Rec(Record {
            pid: 5,
            started_us,
            cwd: cwd.map(PathBuf::from),
        })
    };
    let cases: &[(&[u8], RecordRead)] = &[
        (
            br#"{"pid": 5, "cmd": "x", "cwd": "/y", "started": 1729.5}"#,
            five(1_729_500_000, Some("/y")),
        ),
        (b"", RecordRead::Empty),
        (
            br#"{"pid": 5, "cmd": "x", "cwd": "/y", "started": 1729.5"#,
            RecordRead::Bad,
        ),
        (br#"{"pid": 12"#, RecordRead::Bad),
        (br#"{"pid": "5", "started": 1}"#, RecordRead::Bad),
        (br#"{"pid": 5.0, "started": 1}"#, RecordRead::Bad),
        (br#"{"pid": 1, "started": 1}"#, RecordRead::Bad),
        (br#"{"pid": 0, "started": 1}"#, RecordRead::Bad),
        (br#"{"pid": -5, "started": 1}"#, RecordRead::Bad),
        (br#"{"pid": 2147483648, "started": 1}"#, RecordRead::Bad),
        (
            br#"{"pid": 2147483647, "started": 1}"#,
            RecordRead::Rec(Record {
                pid: i32::MAX as u32,
                started_us: 1_000_000,
                cwd: None,
            }),
        ),
        (br#"{"pid": 5}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": NaN}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": -1}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": 1e20}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": 1e308}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": "1"}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": 1, "v": 2}"#, RecordRead::Bad),
        (
            br#"{"pid": 5, "started": 1, "v": 1}"#,
            five(1_000_000, None),
        ),
        (br#"{"pid": 5, "started": 1, "cmd": 7}"#, RecordRead::Bad),
        (br#"{"pid": 5, "started": 1, "cwd": null}"#, RecordRead::Bad),
        (br#"[5, 1]"#, RecordRead::Bad),
        (b"5", RecordRead::Bad),
        (
            br#"{"pid": 5, "started": 1, "host": "m2", "extra": [1, {"a": null}]}"#,
            five(1_000_000, None),
        ),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            &Record::parse(bytes),
            want,
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn slot_files_are_named_slot_and_digits() {
    let cases = [
        ("slot-0", Some(0)),
        ("slot-12", Some(12)),
        ("slot-999999999", Some(999_999_999)),
        ("slot-", None),
        ("slot-+1", None),
        ("slot-1a", None),
        ("slot-0000000001", None),
        ("slots-1", None),
        ("orphan.pid", None),
    ];
    for (name, want) in cases {
        assert_eq!(slot_index(name), want, "{name}");
    }
}

#[test]
fn a_slot_is_held_only_when_the_same_holder_is_seen_on_two_reads() {
    use Sight::Sighted;
    let mut book = SlotBook::default();
    let first = book.judge(obs(0, &[Sighted(holder(40))]), &CALM, &mut never);
    assert_eq!(
        first,
        SlotsRead::Read {
            counts: SlotCounts {
                n: 1,
                pending: 1,
                ..SlotCounts::default()
            },
            holders: vec![],
        },
        "a first read is pending"
    );
    let second = book.judge(obs(1_000, &[Sighted(holder(40))]), &CALM, &mut never);
    assert_eq!(
        second,
        SlotsRead::Read {
            counts: SlotCounts {
                n: 1,
                held: 1,
                ..SlotCounts::default()
            },
            holders: vec![holder(40)],
        }
    );
    let other = Sighted(Holder {
        pid: 41,
        birth_us: NOW,
    });
    let third = book.judge(obs(2_000, std::slice::from_ref(&other)), &CALM, &mut never);
    assert_eq!(counts_of(&third).pending, 1);
    let fourth = book.judge(obs(3_000, &[other]), &CALM, &mut never);
    assert_eq!(counts_of(&fourth).held, 1);

    for file in [(2, 100), (1, 101)] {
        let sight = Sighted(holder(40));
        let mut book = SlotBook::default();
        book.judge(obs(0, std::slice::from_ref(&sight)), &CALM, &mut never);
        let mut replaced = obs(1_000, &[sight]);
        replaced.slots[0].file = file;
        let first = book.judge(replaced.clone(), &CALM, &mut never);
        assert_eq!(
            (counts_of(&first).pending, counts_of(&first).held),
            (1, 0),
            "{file:?}"
        );
        replaced.mono_ms = 2_000;
        let second = book.judge(replaced, &CALM, &mut never);
        assert_eq!(counts_of(&second).held, 1, "{file:?}");
    }
}

#[test]
fn a_read_closer_than_the_gap_is_not_kept() {
    let mut book = SlotBook::default();
    let first = book.judge(obs(0, &[Sight::Sighted(holder(40))]), &CALM, &mut never);
    let early = book.judge(obs(999, &[Sight::Empty]), &CALM, &mut never);
    assert_eq!(early, first);
    let paired = book.judge(obs(1_000, &[Sight::Sighted(holder(40))]), &CALM, &mut never);
    assert_eq!(counts_of(&paired).held, 1, "{paired:?}");
}

#[test]
fn a_free_slot_needs_two_reads_with_no_holder() {
    let mut book = SlotBook::default();
    let not_held = Sight::NotHeld { gone: None };
    let first = book.judge(obs(0, &[not_held, Sight::Empty]), &CALM, &mut never);
    assert_eq!(
        counts_of(&first),
        SlotCounts {
            n: 2,
            pending: 2,
            ..SlotCounts::default()
        }
    );
    let second = book.judge(obs(1_000, &[Sight::Empty, gone(9, NOW)]), &CALM, &mut never);
    assert_eq!(
        counts_of(&second),
        SlotCounts {
            n: 2,
            free: 2,
            ..SlotCounts::default()
        }
    );

    for file in [(2, 100), (1, 101)] {
        let mut book = SlotBook::default();
        book.judge(obs(0, &[Sight::NotHeld { gone: None }]), &CALM, &mut never);
        let mut replaced = obs(1_000, &[Sight::Empty]);
        replaced.slots[0].file = file;
        let first = book.judge(replaced.clone(), &CALM, &mut never);
        assert_eq!(
            (counts_of(&first).pending, counts_of(&first).free),
            (1, 0),
            "{file:?}"
        );
        replaced.mono_ms = 2_000;
        let second = book.judge(replaced, &CALM, &mut never);
        assert_eq!(counts_of(&second).free, 1, "{file:?}");
    }
}

#[test]
fn a_probers_single_sighting_is_never_a_holder() {
    let mut book = SlotBook::default();
    let seen = [
        Sight::Sighted(holder(40)),
        Sight::NotHeld { gone: None },
        Sight::NotHeld { gone: None },
    ];
    let states: Vec<SlotCounts> = seen
        .iter()
        .zip([0, 1_000, 2_000])
        .map(|(s, at)| counts_of(&book.judge(obs(at, std::slice::from_ref(s)), &CALM, &mut never)))
        .collect();
    assert_eq!(
        states
            .iter()
            .map(|c| (c.held, c.pending, c.free))
            .collect::<Vec<_>>(),
        [(0, 1, 0), (0, 1, 0), (0, 0, 1)]
    );
}

#[test]
fn an_unreadable_record_is_unknown_never_free() {
    let mut book = SlotBook::default();
    let first = book.judge(obs(0, &[Sight::Bad, Sight::Empty]), &CALM, &mut never);
    assert_eq!(
        counts_of(&first),
        SlotCounts {
            n: 2,
            pending: 1,
            unknown: 1,
            ..SlotCounts::default()
        }
    );
    let second = book.judge(obs(1_000, &[Sight::Bad, Sight::Empty]), &CALM, &mut never);
    assert_eq!(
        counts_of(&second),
        SlotCounts {
            n: 2,
            free: 1,
            unknown: 1,
            ..SlotCounts::default()
        }
    );
    let third = book.judge(obs(2_000, &[Sight::Empty, Sight::Empty]), &CALM, &mut never);
    assert_eq!((counts_of(&third).pending, counts_of(&third).free), (1, 1));
    let all_bad = SlotBook::default().judge(obs(0, &[Sight::Bad, Sight::Bad]), &CALM, &mut never);
    assert_eq!(all_bad, SlotsRead::Unread(Unread::UnreadFormat));
    let unproven = SlotBook::default().judge(obs(0, &[Sight::Unproven]), &CALM, &mut never);
    assert_eq!(counts_of(&unproven).unknown, 1);
    let none = SlotBook::default().judge(obs(0, &[]), &CALM, &mut never);
    assert_eq!(none, SlotsRead::Unread(Unread::NotSampled));

    let absent = [Sight::Empty, Sight::NotHeld { gone: None }, gone(9, NOW)];
    for lock in [None, Some(true)] {
        let sights: Vec<_> = absent
            .iter()
            .cloned()
            .map(|s| lock_checked(s, lock))
            .collect();
        assert_eq!(sights, vec![Sight::Unproven; 3], "{lock:?}");
        let mut book = SlotBook::default();
        book.judge(obs(0, &sights), &CALM, &mut never);
        let read = book.judge(obs(1_000, &sights), &CALM, &mut never);
        assert_eq!(
            counts_of(&read),
            SlotCounts {
                n: 3,
                unknown: 3,
                ..SlotCounts::default()
            },
            "{lock:?}"
        );
    }
    let unlocked: Vec<_> = absent
        .iter()
        .cloned()
        .chain([Sight::Unproven])
        .map(|s| lock_checked(s, Some(false)))
        .collect();
    assert_eq!(&unlocked[..3], &absent);
    assert_eq!(unlocked[3], Sight::NotHeld { gone: None });
    let mut book = SlotBook::default();
    book.judge(obs(0, &unlocked), &CALM, &mut never);
    assert_eq!(
        counts_of(&book.judge(obs(1_000, &unlocked), &CALM, &mut never)),
        SlotCounts {
            n: 4,
            free: 4,
            ..SlotCounts::default()
        }
    );
    for lock in [None, Some(false), Some(true)] {
        let sight = lock_checked(Sight::Sighted(holder(40)), lock);
        assert_eq!(sight, Sight::Sighted(holder(40)));
        let mut book = SlotBook::default();
        book.judge(obs(0, std::slice::from_ref(&sight)), &CALM, &mut never);
        let read = book.judge(obs(1_000, &[sight]), &CALM, &mut never);
        assert_eq!(counts_of(&read).held, 1, "{lock:?}");
    }
}

/// The counts of two reads of `sights` under `m`.
fn discounted_by(sights: &[Sight], m: Machine, witnessed: &[u32]) -> (SlotCounts, Vec<u32>) {
    let mut asked = Vec::new();
    let mut witness = |r: &Record| {
        asked.push(r.pid);
        witnessed.contains(&r.pid)
    };
    let mut book = SlotBook::default();
    book.judge(obs(0, sights), &m, &mut witness);
    let read = book.judge(obs(1_000, sights), &m, &mut witness);
    (counts_of(&read), asked)
}

#[test]
fn a_gone_writers_free_slot_is_discounted_only_past_load_horizon_and_a_witness() {
    let at = |load5| Machine {
        load5: Some(load5),
        ncpu: Some(18),
    };
    let minute_ago = NOW - 60_000_000;
    let one = [
        Sight::Sighted(holder(40)),
        gone(10, minute_ago),
        Sight::Empty,
    ];
    let (c, asked) = discounted_by(&one, at(12.5), &[10]);
    assert_eq!((c.held, c.free, c.discounted), (1, 2, 1));
    assert_eq!(asked, [10], "asked once, on the judging read");
    let (c, asked) = discounted_by(&one, at(12.0), &[10]);
    assert_eq!((c.discounted, asked.len()), (0, 0));
    assert_eq!(discounted_by(&one, at(12.5), &[]).0.discounted, 0);
    for m in [
        Machine {
            load5: None,
            ncpu: Some(18),
        },
        Machine {
            load5: Some(1e4),
            ncpu: None,
        },
        Machine {
            load5: Some(1e4),
            ncpu: Some(0),
        },
        Machine {
            load5: Some(f64::NAN),
            ncpu: Some(18),
        },
    ] {
        let (c, asked) = discounted_by(&one, m, &[10]);
        assert_eq!((c.free, c.discounted, asked.len()), (2, 0, 0), "{m:?}");
    }
    let old = [gone(10, NOW - ORPHAN_HORIZON_US - 1), Sight::Empty];
    let (c, asked) = discounted_by(&old, BUSY, &[10]);
    assert_eq!((c.free, c.discounted, asked.len()), (2, 0, 0));
    let edge = [gone(10, NOW - ORPHAN_HORIZON_US), Sight::Empty];
    assert_eq!(discounted_by(&edge, BUSY, &[10]).0.discounted, 1);
    let let_go = [Sight::NotHeld { gone: None }, Sight::Empty];
    assert_eq!(discounted_by(&let_go, BUSY, &[]).1, Vec::<u32>::new());
    let two = [
        gone(10, minute_ago),
        gone(11, minute_ago + 30_000_000),
        Sight::Empty,
    ];
    assert_eq!(
        discounted_by(&two, at(12.5), &[10, 11]),
        (
            SlotCounts {
                n: 3,
                free: 3,
                discounted: 2,
                ..SlotCounts::default()
            },
            vec![11, 10]
        )
    );
    let (c, asked) = discounted_by(&two, at(11.0), &[10, 11]);
    assert_eq!((c.discounted, asked), (1, vec![11]));
    let (c, asked) = discounted_by(&two, at(11.0), &[10]);
    assert_eq!((c.discounted, asked), (1, vec![11, 10]));
}

#[test]
fn a_witness_was_adopted_by_launchd_leads_its_group_and_was_born_in_the_window() {
    let started = NOW;
    let r = rec(9, started);
    let row = |birth_us: u64| crate::kproc::Record {
        pid: 500,
        ppid: 1,
        pgid: 500,
        tpgid: 0,
        tty: None,
        uid: 501,
        comm: "cargo".to_owned(),
        start_s: birth_us / 1_000_000,
        start_us: (birth_us % 1_000_000) as u32,
        zombie: false,
    };
    let births = [
        (started + 2_000_000, true),
        (started, true),
        (started - BIRTH_SLACK_US, true),
        (started - BIRTH_SLACK_US - 1, false),
        (started + WITNESS_WINDOW_US, true),
        (started + WITNESS_WINDOW_US + 1, false),
    ];
    for (birth, want) in births {
        assert_eq!(adopted_in_window(&row(birth), &r, 501), want, "{birth}");
    }
    let born = started + 1_000_000;
    let others = [
        crate::kproc::Record {
            ppid: 77,
            ..row(born)
        },
        crate::kproc::Record {
            pgid: 499,
            ..row(born)
        },
        crate::kproc::Record {
            uid: 0,
            ..row(born)
        },
        crate::kproc::Record {
            zombie: true,
            ..row(born)
        },
    ];
    for p in &others {
        assert!(!adopted_in_window(p, &r, 501), "{p:?}");
    }
}

#[cfg(target_os = "macos")]
mod live {
    use std::fs::OpenOptions;
    use std::io::{Read as _, Write as _};
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use aterm_uds::spawnfd::{Child, Stdio};

    use super::super::live::observe_in;
    use super::super::*;
    use super::{BUSY, CALM, counts_of, never, rec};
    use crate::kproc;

    /// This test, run alone in a child.
    const STAND_IN: &str = "slots::tests::live::stand_in";
    /// The slot file the stand-in takes.
    const SLOT_ENV: &str = "ATERM_SLOT_STAND_IN";
    /// `holder` asks for a build.
    const BUILD_ENV: &str = "ATERM_SLOT_STAND_IN_BUILD";
    /// The hang detector for what must happen (a child's line, a reap).
    const HANG: Duration = Duration::from_secs(60);

    fn now_us() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("after the epoch")
                .as_micros(),
        )
        .expect("fits")
    }

    /// A record as `build-slot` spells it (Python's `json.dumps`).
    fn python_record(pid: u32, cwd: &Path, started_us: u64) -> String {
        let s = |v: &str| aterm_json::to_string(v).expect("a JSON string");
        let started = started_us as f64 / 1e6;
        format!(
            "{{\"pid\": {pid}, \"cmd\": {}, \"cwd\": {}, \"started\": {started:?}}}",
            s("targo --unverified build"),
            s(cwd.to_str().expect("a UTF-8 path")),
        )
    }

    fn look(dir: &Path, mono_ms: u64) -> Observation {
        observe(dir, mono_ms, now_us()).expect("the slots read")
    }

    fn birth(pid: u32) -> Option<u64> {
        kproc::record(pid)
            .filter(|p| !p.zombie)
            .map(|p| super::super::birth_us(&p))
    }

    /// Wait, under [`HANG`], until `done`.
    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + HANG;
        while !done() {
            assert!(Instant::now() < deadline, "hang detector: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "the build-slot holder the live tests start in a child; alone it returns"]
    fn stand_in() {
        let Some(slot) = std::env::var_os(SLOT_ENV) else {
            return;
        };
        let slot = Path::new(&slot);
        if std::env::var(BUILD_ENV).as_deref() == Ok("build") {
            std::fs::write(
                slot.with_file_name("build.pid"),
                std::process::id().to_string(),
            )
            .expect("write build.pid");
            println!("\nbuild-held");
            std::io::stdout().flush().expect("the build's proof");
            let mut byte = [0; 1];
            loop {
                match std::io::stdin().read(&mut byte) {
                    Ok(0) => aterm_uds::process::kill_self_now(),
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => aterm_uds::process::kill_self_now(),
                }
            }
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(slot)
            .expect("open the slot as build-slot does");
        #[expect(
            clippy::disallowed_methods,
            reason = "the stand-in plays build-slot's holder: it takes the slot's flock as \
                      build-slot does and keeps it until it is SIGKILLed, when the kernel drops it"
        )]
        let locked = file.try_lock();
        locked.expect("the slot is free");
        file.set_len(0).expect("truncate");
        let cwd = std::env::current_dir().expect("the working directory");
        file.write_all(python_record(std::process::id(), &cwd, now_us()).as_bytes())
            .expect("write the record");
        let _build = std::env::var_os(BUILD_ENV).map(|_| {
            let child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test image"))
                .args([
                    "--exact",
                    STAND_IN,
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(BUILD_ENV, "build")
                .setsid(true)
                .stdin(Stdio::inherit())
                .stdout(Stdio::piped())
                .spawn()
                .expect("the build starts");
            let mut build = ChildGuard(Some(child));
            aterm_uds::deadline::prints(build.0.as_mut().expect("build"), "build-held", HANG);
            build
        });
        println!("\nheld");
        loop {
            std::thread::park();
        }
    }

    /// Direct-child custody, armed before readiness can allocate a reader.
    struct ChildGuard(Option<Child>);

    impl ChildGuard {
        fn end(&mut self) -> Result<(), String> {
            let Some(child) = self.0.as_mut() else {
                return Ok(());
            };
            let _ = child.kill();
            let mut exit = aterm_uds::deadline::ExitWait::new(child);
            let end = Instant::now() + HANG;
            loop {
                if let Some(status) = child.try_wait().map_err(|e| format!("reap: {e}"))? {
                    self.0 = None;
                    return if status.signal() == Some(libc::SIGKILL) {
                        Ok(())
                    } else {
                        Err(format!("the direct child did not end by SIGKILL: {status}"))
                    };
                }
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err("direct child not reaped after SIGKILL".into());
                }
                exit.wait(left);
            }
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Err(why) = self.end() {
                eprintln!("slot fixture cleanup unknown: {why}");
            }
        }
    }

    /// A wrapper, and its build's stop authority armed before either starts.
    struct Holding {
        child: ChildGuard,
        build: Option<Build>,
        world: Arc<aterm_tempfile::TempDir>,
    }

    impl Holding {
        /// Start the stand-in on `slot`, working in `dir`, optionally with a build.
        fn start(dir: &Arc<aterm_tempfile::TempDir>, slot: &Path, build: bool) -> Self {
            Self::start_with(dir, slot, build, |_| {})
        }

        fn start_with(
            dir: &Arc<aterm_tempfile::TempDir>,
            slot: &Path,
            build: bool,
            before_ready: impl FnOnce(&mut Child),
        ) -> Self {
            let mut holding = Self {
                child: ChildGuard(None),
                build: None,
                world: Arc::clone(dir),
            };
            let mut cmd =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
            cmd.args([
                "--exact",
                STAND_IN,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SLOT_ENV, slot)
            .env_remove(BUILD_ENV)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
            if build {
                let (stop, far) = UnixStream::pair().expect("the build's stop authority");
                stop.set_nonblocking(true)
                    .expect("nonblocking build witness");
                holding.build = Some(Build {
                    stop,
                    pid: 0,
                    watch: None,
                    eof: false,
                    done: false,
                    world: Arc::clone(dir),
                });
                cmd.env(BUILD_ENV, "holder")
                    .stdin(Stdio::from(OwnedFd::from(far)));
            }
            holding.child = ChildGuard(Some(cmd.spawn().expect("the stand-in starts")));
            let child = holding.child.0.as_mut().expect("the wrapper");
            before_ready(child);
            aterm_uds::deadline::prints(child, "held", HANG);
            if let Some(build) = &mut holding.build {
                build.confirm(slot);
            }
            holding
        }

        fn pid(&self) -> u32 {
            self.child.0.as_ref().map_or(0, Child::id)
        }

        /// `SIGKILL` and reap it (once).
        fn end(&mut self) {
            self.child.end().expect("SIGKILL and reap the stand-in");
        }
    }

    impl Drop for Holding {
        fn drop(&mut self) {
            let direct = self.child.end();
            let build = self.build.as_mut().map(Build::end).transpose();
            if direct.is_err() || build.is_err() {
                eprintln!(
                    "KEPT {}: fixture cleanup unknown: {direct:?}, {build:?}",
                    self.world.path().display()
                );
                std::mem::forget(Arc::clone(&self.world));
            }
        }
    }

    /// The adopted build stops itself on this exclusively owned endpoint's EOF.
    struct Build {
        stop: UnixStream,
        pid: u32,
        watch: Option<aterm_uds::exitwatch::ExitWatch>,
        eof: bool,
        done: bool,
        world: Arc<aterm_tempfile::TempDir>,
    }

    impl Build {
        fn confirm(&mut self, slot: &Path) {
            self.pid = std::fs::read_to_string(slot.with_file_name("build.pid"))
                .expect("the build's own pid")
                .trim()
                .parse()
                .expect("a pid");
            let born = birth(self.pid).expect("the build is alive");
            self.watch = aterm_uds::exitwatch::ExitWatch::watch(self.pid);
            assert!(self.watch.is_some(), "the kernel exit watch");
            assert_eq!(
                birth(self.pid),
                Some(born),
                "same build across registration"
            );
        }

        fn end(&mut self) -> Result<(), String> {
            if self.done {
                return Ok(());
            }
            let _ = self.stop.shutdown(std::net::Shutdown::Write);
            let end = Instant::now() + HANG;
            loop {
                let mut byte = [0; 1];
                match self.stop.read(&mut byte) {
                    Ok(0) => self.eof = true,
                    Ok(_) => return Err("unexpected build witness bytes".into()),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(format!("build witness: {e}")),
                }
                let status = self.watch.as_ref().and_then(|w| w.exit_status());
                if self.eof
                    && let Some(raw) = status
                {
                    let status = ExitStatus::from_raw(raw);
                    if status.signal() != Some(libc::SIGKILL) {
                        return Err(format!("the build did not end by SIGKILL: {status}"));
                    }
                    self.done = true;
                    return Ok(());
                }
                if Instant::now() >= end {
                    return Err(
                        "build endpoint EOF and kernel exit status not both confirmed".into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Build {
        fn drop(&mut self) {
            if let Err(why) = self.end() {
                eprintln!(
                    "KEPT {}: build cleanup unknown: {why}",
                    self.world.path().display()
                );
                std::mem::forget(Arc::clone(&self.world));
            }
        }
    }

    #[test]
    fn a_stand_in_holds_slot_0_then_dies_by_sigkill() {
        let dir = Arc::new(aterm_tempfile::tempdir().expect("scratch dir"));
        let slot = dir.path().join("slot-0");
        let mut holding = Holding::start(&dir, &slot, false);
        let pid = holding.pid();
        let mut book = SlotBook::default();
        let first = book.judge(look(dir.path(), 0), &BUSY, &mut live_witness());
        assert_eq!(
            counts_of(&first),
            SlotCounts {
                n: 1,
                pending: 1,
                ..SlotCounts::default()
            },
            "one sighting is not a holder"
        );
        let second = book.judge(look(dir.path(), 1_000), &BUSY, &mut live_witness());
        let SlotsRead::Read { counts, holders } = &second else {
            panic!("{second:?}");
        };
        assert_eq!(
            (
                counts.held,
                holders.iter().map(|h| h.pid).collect::<Vec<_>>()
            ),
            (1, vec![pid]),
            "{second:?}"
        );
        assert_eq!(Some(holders[0].birth_us), birth(pid));
        holding.end();
        let after = look(dir.path(), 2_000);
        assert!(
            matches!(&after.slots[0].sight, Sight::NotHeld { gone: Some(r) } if r.pid == pid),
            "{after:?}"
        );
        assert_eq!(
            counts_of(&book.judge(after, &BUSY, &mut live_witness())).pending,
            1
        );
        let freed = book.judge(look(dir.path(), 3_000), &BUSY, &mut live_witness());
        assert_eq!(
            freed,
            SlotsRead::Read {
                counts: SlotCounts {
                    n: 1,
                    free: 1,
                    ..SlotCounts::default()
                },
                holders: vec![],
            }
        );
        let mut watch = None;
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Holding::start_with(&dir, &slot, false, |child| {
                aterm_uds::deadline::prints(child, "held", HANG);
                watch = aterm_uds::exitwatch::ExitWatch::watch(child.id());
                assert!(watch.is_some(), "watch the proven child before unwinding");
                panic!("injected failure before the ordinary readiness check");
            })
        }));
        assert!(failed.is_err(), "the injected constructor failure");
        let watch = watch.expect("the child wrote its proof before the failure");
        until("the failed constructor's child exits", || {
            watch.exit_status().is_some()
        });
        assert_eq!(
            ExitStatus::from_raw(watch.exit_status().expect("kernel exit status")).signal(),
            Some(libc::SIGKILL)
        );
    }

    #[test]
    fn a_reused_pid_reads_free() {
        let me = std::process::id();
        let born = birth(me).expect("this process's birth");
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slot = dir.path().join("slot-0");
        std::fs::write(&slot, python_record(me, dir.path(), born - 10_000_000))
            .expect("a record older than this process");
        let _open = std::fs::File::open(&slot).expect("hold the slot open");
        let mut book = SlotBook::default();
        let first = look(dir.path(), 0);
        assert!(
            matches!(&first.slots[0].sight, Sight::NotHeld { gone: Some(r) } if r.pid == me),
            "{first:?}"
        );
        book.judge(first, &CALM, &mut never);
        let read = book.judge(look(dir.path(), 1_000), &CALM, &mut never);
        assert_eq!(
            counts_of(&read),
            SlotCounts {
                n: 1,
                free: 1,
                ..SlotCounts::default()
            }
        );
        std::fs::write(&slot, python_record(me, dir.path(), born + 1_000_000))
            .expect("a record younger than this process");
        let mut book = SlotBook::default();
        book.judge(look(dir.path(), 0), &CALM, &mut never);
        let held = book.judge(look(dir.path(), 1_000), &CALM, &mut never);
        assert_eq!(
            held,
            SlotsRead::Read {
                counts: SlotCounts {
                    n: 1,
                    held: 1,
                    ..SlotCounts::default()
                },
                holders: vec![Holder {
                    pid: me,
                    birth_us: born
                }],
            }
        );
    }

    #[test]
    fn an_unparsable_record_reads_unknown_never_free() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        std::fs::write(dir.path().join("slot-0"), br#"{"pid": 12"#).expect("a torn record");
        std::fs::write(dir.path().join("slot-1"), b"").expect("an empty slot");
        let mut book = SlotBook::default();
        book.judge(look(dir.path(), 0), &CALM, &mut never);
        let read = book.judge(look(dir.path(), 1_000), &CALM, &mut never);
        assert_eq!(
            counts_of(&read),
            SlotCounts {
                n: 2,
                free: 1,
                unknown: 1,
                ..SlotCounts::default()
            }
        );
        std::fs::remove_file(dir.path().join("slot-1")).expect("remove the empty slot");
        let only_bad = SlotBook::default().judge(look(dir.path(), 0), &CALM, &mut never);
        assert_eq!(only_bad, SlotsRead::Unread(Unread::UnreadFormat));
        std::fs::write(dir.path().join("slot-0"), vec![b' '; RECORD_CAP + 1]).expect("too long");
        std::fs::create_dir(dir.path().join("slot-1")).expect("a directory slot");
        let o = look(dir.path(), 0);
        assert_eq!(
            o.slots.iter().map(|s| &s.sight).collect::<Vec<_>>(),
            [&Sight::Bad, &Sight::Bad]
        );
    }

    #[test]
    fn a_symlinked_slot_is_not_followed() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let target = dir.path().join("elsewhere");
        let me = std::process::id();
        let born = birth(me).expect("this process's birth");
        std::fs::write(&target, python_record(me, dir.path(), born + 1_000_000)).expect("a record");
        let _open = std::fs::File::open(&target).expect("hold it open");
        std::os::unix::fs::symlink(&target, dir.path().join("slot-0")).expect("symlink");
        let o = look(dir.path(), 0);
        assert_eq!(o.slots.len(), 1);
        assert_eq!(o.slots[0].sight, Sight::Bad, "{o:?}");
    }

    #[test]
    fn a_probers_single_sighting_reads_pending_then_free() {
        let me = std::process::id();
        let born = birth(me).expect("this process's birth");
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slot = dir.path().join("slot-0");
        std::fs::write(&slot, python_record(me, dir.path(), born + 1_000_000)).expect("a record");
        let mut book = SlotBook::default();
        let open = std::fs::File::open(&slot).expect("the prober opens it");
        let seen = look(dir.path(), 0);
        assert!(
            matches!(seen.slots[0].sight, Sight::Sighted(h) if h.pid == me),
            "{seen:?}"
        );
        let first = book.judge(seen, &CALM, &mut never);
        drop(open);
        let let_go = look(dir.path(), 1_000);
        assert_eq!(let_go.slots[0].sight, Sight::NotHeld { gone: None });
        let second = book.judge(let_go, &CALM, &mut never);
        let third = book.judge(look(dir.path(), 2_000), &CALM, &mut never);
        assert_eq!(
            [&first, &second, &third].map(|r| {
                let c = counts_of(r);
                (c.held, c.pending, c.free)
            }),
            [(0, 1, 0), (0, 1, 0), (0, 0, 1)]
        );
    }

    #[test]
    fn a_dead_wrappers_live_build_is_discounted() {
        let dir = Arc::new(aterm_tempfile::tempdir().expect("scratch dir"));
        let slot = dir.path().join("slot-0");
        let mut holding = Holding::start(&dir, &slot, true);
        let mut build = holding
            .build
            .take()
            .expect("owned before the wrapper started");
        holding.end();
        until("launchd adopts the build", || {
            kproc::record(build.pid).is_some_and(|p| p.ppid == 1)
        });
        let mut book = SlotBook::default();
        book.judge(look(dir.path(), 0), &BUSY, &mut live_witness());
        let occupied = book.judge(look(dir.path(), 1_000), &BUSY, &mut live_witness());
        assert_eq!(
            counts_of(&occupied),
            SlotCounts {
                n: 1,
                free: 1,
                discounted: 1,
                ..SlotCounts::default()
            },
            "a load no slot explains, and the build runs on"
        );
        let RecordRead::Rec(first) =
            Record::parse(&std::fs::read(&slot).expect("the stand-in's record"))
        else {
            panic!("the stand-in's record parses");
        };
        let second = dir.path().join("slot-1");
        let cwd = first
            .cwd
            .as_deref()
            .expect("the stand-in's record names its cwd");
        std::fs::write(
            &second,
            python_record(first.pid, cwd, first.started_us - 1_000_000),
        )
        .expect("a second gone writer's record");
        let mut two = SlotBook::default();
        two.judge(look(dir.path(), 0), &BUSY, &mut live_witness());
        let once = two.judge(look(dir.path(), 1_000), &BUSY, &mut live_witness());
        assert_eq!(
            counts_of(&once),
            SlotCounts {
                n: 2,
                free: 2,
                discounted: 1,
                ..SlotCounts::default()
            },
            "one build witnesses one slot"
        );
        std::fs::remove_file(&second).expect("remove the second record");
        let calm = book.judge(look(dir.path(), 2_000), &CALM, &mut live_witness());
        assert_eq!((counts_of(&calm).free, counts_of(&calm).discounted), (1, 0));
        build
            .end()
            .expect("the build's EOF and kernel SIGKILL status");
        let gone = book.judge(look(dir.path(), 3_000), &BUSY, &mut live_witness());
        assert_eq!((counts_of(&gone).free, counts_of(&gone).discounted), (1, 0));
    }

    #[test]
    fn a_concurrent_acquirer_never_meets_a_busy_free_slot() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slot = dir.path().join("slot-0");
        std::fs::write(&slot, b"").expect("a free slot, as `status` leaves one");
        let before = std::fs::symlink_metadata(&slot).expect("the slot");
        let open = |path: &Path| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .expect("open as build-slot does")
        };
        let mut acquired = None;
        observe_in(dir.path(), 0, now_us(), &mut |path| {
            let acquirer = open(path);
            #[expect(
                clippy::disallowed_methods,
                reason = "the test plays build-slot's acquirer while the reader's descriptor is \
                          open; it lets go by LOCK_UN below"
            )]
            let got = acquirer.try_lock();
            acquired = Some(got.is_ok());
            acquirer.unlock().expect("LOCK_UN");
        })
        .expect("the slots read");
        assert_eq!(acquired, Some(true), "the reader holds no lock");
        let mut blocked = None;
        observe_in(dir.path(), 1_000, now_us(), &mut |path| {
            let status = open(path);
            #[expect(
                clippy::disallowed_methods,
                reason = "the test plays build-slot's `status`, which locks a free slot to see \
                          it is free; it lets go by LOCK_UN below"
            )]
            let probed = status.try_lock();
            probed.expect("the free slot");
            let acquirer = open(path);
            #[expect(
                clippy::disallowed_methods,
                reason = "the test plays build-slot's acquirer; it holds nothing to let go"
            )]
            let got = acquirer.try_lock();
            blocked = Some(matches!(got, Err(std::fs::TryLockError::WouldBlock)));
            status.unlock().expect("LOCK_UN");
        })
        .expect("the slots read");
        assert_eq!(blocked, Some(true), "the control can fail");
        assert!(std::fs::symlink_metadata(dir.path().join("slot-1")).is_err());
        let after = std::fs::symlink_metadata(&slot).expect("the slot");
        assert_eq!(
            (
                after.ino(),
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
                after.mode()
            ),
            (
                before.ino(),
                before.len(),
                before.mtime(),
                before.mtime_nsec(),
                before.mode()
            )
        );
    }

    #[test]
    fn this_process_reads_its_own_open_files() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("open");
        let file = std::fs::File::create(&path).expect("a file");
        let meta = file.metadata().expect("its identity");
        let id = (meta.dev() as u32, meta.ino());
        let (ids, _) = crate::jobs::macos_vnode_ids(std::process::id()).expect("own descriptors");
        assert!(ids.contains(&id), "{id:?} in {ids:?}");
        drop(file);
        let (ids, _) = crate::jobs::macos_vnode_ids(std::process::id()).expect("own descriptors");
        assert!(!ids.contains(&id), "closed: {id:?} in {ids:?}");
    }

    #[test]
    fn a_reader_hands_back_its_reading_until_the_period_passes() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slots = dir.path().join("build-slot");
        let mut reader = SlotReader::new(slots.clone());
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert_eq!(reader.read_at(), None);
        assert_eq!(
            reader.read(at(0), now_us(), &CALM, true),
            SlotsRead::Unread(Unread::NotSampled)
        );
        assert_eq!(reader.read_at(), Some(at(0)));
        std::fs::create_dir(&slots).expect("the slot dir");
        std::fs::write(slots.join("slot-0"), b"").expect("a free slot");
        assert_eq!(
            reader.read(at(1), now_us(), &CALM, true),
            SlotsRead::Unread(Unread::NotSampled),
            "within the period: the last reading"
        );
        let pending = reader.read(at(5), now_us(), &CALM, true);
        assert_eq!(counts_of(&pending).pending, 1, "{pending:?}");
        let free = reader.read(at(10), now_us(), &CALM, true);
        assert_eq!(counts_of(&free).free, 1, "{free:?}");
        assert_eq!(reader.read_at(), Some(at(10)));
        std::fs::write(slots.join("slot-1"), b"").expect("a second slot");
        for s in [15, 30, 69] {
            assert_eq!(reader.read(at(s), now_us(), &CALM, false), free, "at {s} s");
        }
        assert_eq!(reader.read_at(), Some(at(10)));
        let idle = reader.read(at(70), now_us(), &CALM, false);
        assert_eq!(counts_of(&idle).n, 2, "{idle:?}");
        assert_eq!(reader.read_at(), Some(at(70)));
        std::fs::write(slots.join("slot-2"), b"").expect("a third slot");
        let armed = reader.read(at(75), now_us(), &CALM, true);
        assert_eq!(counts_of(&armed).n, 3, "{armed:?}");
        assert_eq!(
            default_dir(Path::new("/Users//u")),
            Path::new("/Users//u/.cache/build-slot")
        );
    }

    /// A READING THAT MUST BE NEW ([`REREAD`]) is taken once the last is a paired gap old, where
    /// the armed cadence hands the last one back. NEGATIVE CONTROL: the same reads at the armed
    /// period keep the last reading.
    #[test]
    fn a_reread_reads_anew_past_the_paired_gap() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slots = dir.path().join("build-slot");
        std::fs::create_dir(&slots).expect("the slot dir");
        std::fs::write(slots.join("slot-0"), b"").expect("a free slot");
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        for (period, anew) in [(PERIOD, false), (REREAD, true)] {
            let mut reader = SlotReader::new(slots.clone());
            let first = reader.read_within(at(0), now_us(), &CALM, period);
            assert_eq!(counts_of(&first).n, 1, "{first:?}");
            std::fs::write(slots.join("slot-1"), b"").expect("a second slot");
            let inside = reader.read_within(at(999), now_us(), &CALM, period);
            assert_eq!(inside, first, "inside the paired gap: the last reading");
            let past = reader.read_within(at(1_000), now_us(), &CALM, period);
            assert_eq!(counts_of(&past).n, if anew { 2 } else { 1 }, "{period:?}");
            assert_eq!(reader.read_at(), Some(at(if anew { 1_000 } else { 0 })));
            std::fs::remove_file(slots.join("slot-1")).expect("the second slot gone");
        }
    }

    #[test]
    fn a_lock_held_with_no_record_to_show_it_is_never_free() {
        let me = std::process::id();
        let born = birth(me).expect("this process's birth");
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let empty = dir.path().join("slot-0");
        let stale = dir.path().join("slot-1");
        std::fs::write(&empty, b"").expect("truncated, no record yet");
        std::fs::write(&stale, python_record(me, dir.path(), born - 10_000_000))
            .expect("a gone writer's record");
        let open = |path: &Path| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .expect("open as build-slot does")
        };
        let (a, b) = (open(&empty), open(&stale));
        for f in [&a, &b] {
            #[expect(
                clippy::disallowed_methods,
                reason = "the test plays build-slot's winner stalled before its record; it lets \
                          go by LOCK_UN below"
            )]
            let got = f.try_lock();
            got.expect("the slot is free");
        }
        let reader_side = std::fs::File::open(&empty).expect("a reader's descriptor");
        assert_eq!(super::super::live::locked(&reader_side), Some(true));
        let mut book = SlotBook::default();
        let reads: Vec<SlotCounts> = (0..3)
            .map(|i| counts_of(&book.judge(look(dir.path(), i * 1_000), &CALM, &mut never)))
            .collect();
        for c in &reads {
            assert_eq!((c.n, c.free, c.unknown), (2, 0, 2), "{reads:?}");
        }
        for f in [&a, &b] {
            f.unlock().expect("LOCK_UN");
        }
        assert_eq!(super::super::live::locked(&reader_side), Some(false));
        let mut book = SlotBook::default();
        book.judge(look(dir.path(), 0), &CALM, &mut never);
        let freed = book.judge(look(dir.path(), 1_000), &CALM, &mut never);
        assert_eq!(
            counts_of(&freed),
            SlotCounts {
                n: 2,
                free: 2,
                ..SlotCounts::default()
            }
        );
    }

    #[test]
    fn a_refused_descriptor_table_is_unproven_until_the_lock_says() {
        // SAFETY: `geteuid` reads this process's effective uid.
        assert_ne!(unsafe { libc::geteuid() }, 0, "run as a user, not root");
        // launchd: a root process that outlives the test. The first root process the table
        // listed could exit between the reads below (a loaded machine starts and ends short
        // root jobs), and its writer then read gone, not unproven.
        let daemon = kproc::record(1)
            .filter(|p| p.uid == 0 && !p.zombie)
            .expect("launchd, pid 1, a live root process");
        let table = crate::jobs::macos_vnode_ids(daemon.pid);
        assert_eq!(table, None, "a refused table read as a list");
        let writer = rec(daemon.pid, now_us());
        let file = (1, 100);
        let holder = super::super::live::holder;
        assert_eq!(holder(writer.clone(), file, None), Sight::Unproven);
        assert_eq!(
            holder(writer.clone(), file, Some(false)),
            Sight::NotHeld { gone: None }
        );
        assert_eq!(holder(writer, file, Some(true)), Sight::Unproven);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_holder_is_running_only_as_the_process_born_when_it_was_proven() {
    let pid = std::process::id();
    let me = crate::kproc::record(pid).expect("this process's record");
    let holder = Holder {
        pid,
        birth_us: birth_us(&me),
    };
    assert!(holder.running());
    let reborn = Holder {
        birth_us: holder.birth_us + 1,
        ..holder
    };
    assert!(!reborn.running());
}
