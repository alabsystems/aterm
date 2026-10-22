// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build-slot reader: the record parse, the two-read judgement and the
//! orphan rule as tables (every host), and on macOS the live reader against
//! real flocks held by stand-ins of `build-slot` (this test binary re-run in
//! place), each killed by `SIGKILL`. "Seen on two reads" is two explicit reads
//! at injected clock values, never a sleep.

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
    // Another holder: a change, pending; the same twice, held.
    let other = Sighted(Holder {
        pid: 41,
        birth_us: NOW,
    });
    let third = book.judge(obs(2_000, std::slice::from_ref(&other)), &CALM, &mut never);
    assert_eq!(counts_of(&third).pending, 1);
    let fourth = book.judge(obs(3_000, &[other]), &CALM, &mut never);
    assert_eq!(counts_of(&fourth).held, 1);
}

#[test]
fn a_read_closer_than_the_gap_is_not_kept() {
    let mut book = SlotBook::default();
    let first = book.judge(obs(0, &[Sight::Sighted(holder(40))]), &CALM, &mut never);
    // 999 ms later: the first judgement stands, and this read is not kept.
    let early = book.judge(obs(999, &[Sight::Empty]), &CALM, &mut never);
    assert_eq!(early, first);
    // So the next read pairs with the first.
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
    // An empty record and a let-go one pair either way round.
    let second = book.judge(obs(1_000, &[Sight::Empty, gone(9, NOW)]), &CALM, &mut never);
    assert_eq!(
        counts_of(&second),
        SlotCounts {
            n: 2,
            free: 2,
            ..SlotCounts::default()
        }
    );
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
    // Unknown at once, on a first read too.
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
    // A slot that read Bad before and empty now has changed: pending.
    let third = book.judge(obs(2_000, &[Sight::Empty, Sight::Empty]), &CALM, &mut never);
    assert_eq!((counts_of(&third).pending, counts_of(&third).free), (1, 1));
    // Every slot unreadable: the record changed under us.
    let all_bad = SlotBook::default().judge(obs(0, &[Sight::Bad, Sight::Bad]), &CALM, &mut never);
    assert_eq!(all_bad, SlotsRead::Unread(Unread::UnreadFormat));
    // Unproven is unknown too, but says nothing of the format.
    let unproven = SlotBook::default().judge(obs(0, &[Sight::Unproven]), &CALM, &mut never);
    assert_eq!(counts_of(&unproven).unknown, 1);
    let none = SlotBook::default().judge(obs(0, &[]), &CALM, &mut never);
    assert_eq!(none, SlotsRead::Unread(Unread::NotSampled));
}

/// The counts of two reads of `sights` under `m`, and the pids the witness
/// was asked about, in order.
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
    // Three slots, one held: each slot explains 6 of the 18 cores' load.
    let one = [
        Sight::Sighted(holder(40)),
        gone(10, minute_ago),
        Sight::Empty,
    ];
    let (c, asked) = discounted_by(&one, at(12.5), &[10]);
    assert_eq!((c.held, c.free, c.discounted), (1, 2, 1));
    assert_eq!(asked, [10], "asked once, on the judging read");
    // The load the held slot and this orphan explain: not counted.
    let (c, asked) = discounted_by(&one, at(12.0), &[10]);
    assert_eq!((c.discounted, asked.len()), (0, 0));
    // No witness: not counted.
    assert_eq!(discounted_by(&one, at(12.5), &[]).0.discounted, 0);
    // No load or no core count: never.
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
    // Past the horizon: not even asked.
    let old = [gone(10, NOW - ORPHAN_HORIZON_US - 1), Sight::Empty];
    let (c, asked) = discounted_by(&old, BUSY, &[10]);
    assert_eq!((c.free, c.discounted, asked.len()), (2, 0, 0));
    let edge = [gone(10, NOW - ORPHAN_HORIZON_US), Sight::Empty];
    assert_eq!(discounted_by(&edge, BUSY, &[10]).0.discounted, 1);
    // A writer that lives and let go is no candidate.
    let let_go = [Sight::NotHeld { gone: None }, Sight::Empty];
    assert_eq!(discounted_by(&let_go, BUSY, &[]).1, Vec::<u32>::new());
    // Two candidates, the newest asked first; each counted raises the bar.
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
    // A candidate with no witness does not raise the bar for the next.
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
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use aterm_uds::spawnfd::{Child, Stdio};

    use super::super::live::observe_in;
    use super::super::*;
    use super::{BUSY, CALM, counts_of, never, rec};
    use crate::kproc;

    /// This test, run alone in a child: the stand-in holder.
    const STAND_IN: &str = "slots::tests::live::stand_in";
    /// The slot file the stand-in takes.
    const SLOT_ENV: &str = "ATERM_SLOT_STAND_IN";
    /// The program the stand-in starts as its build, in a session of its own.
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

    /// Not a test: the `build-slot` holder the live tests start in a child.
    /// It takes the slot named by [`SLOT_ENV`] as `build-slot` does (open,
    /// `LOCK_EX | LOCK_NB`, truncate, one write of its record), starts the
    /// build [`BUILD_ENV`] names in a session of its own, says `held`, and
    /// waits for its `SIGKILL`.
    #[test]
    #[ignore = "the build-slot holder the live tests start in a child; alone it returns"]
    fn stand_in() {
        let Some(slot) = std::env::var_os(SLOT_ENV) else {
            return;
        };
        let slot = Path::new(&slot);
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
        let _build = std::env::var_os(BUILD_ENV).map(|program| {
            let build = aterm_uds::spawnfd::command(program)
                .arg("600")
                .setsid(true)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the build starts");
            std::fs::write(slot.with_file_name("build.pid"), build.id().to_string())
                .expect("write build.pid");
            build
        });
        // Its own line: libtest has written `test … ... ` before the body.
        println!("\nheld");
        loop {
            std::thread::park();
        }
    }

    /// A stand-in holder; dropped, it is `SIGKILL`ed and reaped.
    struct Holding(Option<Child>);

    impl Holding {
        /// Start the stand-in on `slot`, working in `dir`, with `build` as its
        /// build; back once it holds the slot.
        fn start(dir: &Path, slot: &Path, build: Option<&Path>) -> Self {
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
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
            if let Some(build) = build {
                cmd.env(BUILD_ENV, build);
            }
            let mut child = cmd.spawn().expect("the stand-in starts");
            aterm_uds::deadline::prints(&mut child, "held", HANG);
            Self(Some(child))
        }

        fn pid(&self) -> u32 {
            self.0.as_ref().map_or(0, Child::id)
        }

        /// `SIGKILL` and reap it (once).
        fn end(&mut self) {
            if let Some(mut child) = self.0.take() {
                child.kill().expect("SIGKILL the stand-in");
                child.wait().expect("reap the stand-in");
            }
        }
    }

    impl Drop for Holding {
        fn drop(&mut self) {
            self.end();
        }
    }

    /// The stand-in's build, which launchd adopts once its parent dies; dropped,
    /// it is `SIGKILL`ed while it is still the process this test started.
    struct Build {
        pid: u32,
        birth: u64,
    }

    impl Build {
        fn kill(&self) {
            if birth(self.pid) == Some(self.birth)
                && let Ok(pid) = i32::try_from(self.pid)
            {
                // SAFETY: a signal to the process this test started (its pid
                // and birth checked just now).
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    impl Drop for Build {
        fn drop(&mut self) {
            self.kill();
        }
    }

    #[test]
    fn a_stand_in_holds_slot_0_then_dies_by_sigkill() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slot = dir.path().join("slot-0");
        let mut holding = Holding::start(dir.path(), &slot, None);
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
        // Free, and at any load not occupied: the stand-in started no build.
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
    }

    #[test]
    fn a_reused_pid_reads_free() {
        let me = std::process::id();
        let born = birth(me).expect("this process's birth");
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slot = dir.path().join("slot-0");
        std::fs::write(&slot, python_record(me, dir.path(), born - 10_000_000))
            .expect("a record older than this process");
        // This process holds the file open, so only its birth rules it out.
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
        // Negative control: a record written after this process was born.
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
        // A record past the cap, and a slot that is no regular file.
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
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let slot = dir.path().join("slot-0");
        let mut holding = Holding::start(dir.path(), &slot, Some(&sleep));
        let pid: u32 = std::fs::read_to_string(dir.path().join("build.pid"))
            .expect("build.pid")
            .trim()
            .parse()
            .expect("a pid");
        let build = Build {
            pid,
            birth: birth(pid).expect("the build runs"),
        };
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
        // ONE WITNESS, ONE SLOT: a second gone writer's record in the same
        // working directory (the first's, as its writer spelled it), started
        // a second before the first (so the build was born in its window
        // too), is not discounted by the same build again.
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
        // A quiet machine needs no orphan to explain it.
        let calm = book.judge(look(dir.path(), 2_000), &CALM, &mut live_witness());
        assert_eq!((counts_of(&calm).free, counts_of(&calm).discounted), (1, 0));
        // Negative control: the build gone, the same load finds no witness.
        build.kill();
        until("the build is reaped", || {
            birth(build.pid) != Some(build.birth)
        });
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
        // Negative control: a reader that locks a free slot as `status` does
        // makes the same acquirer fail.
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
        // The reader created nothing and changed nothing.
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

    /// The reader's cadence: the last reading is handed back until a period
    /// passes, [`PERIOD`] while a wait names the slots (`armed`), and
    /// [`IDLE_PERIOD`] otherwise; [`SlotReader::read_at`] says when it was
    /// read. NEGATIVE CONTROL: the same reads armed re-read at [`PERIOD`].
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
        // Nothing armed: the reading stands for a minute.
        std::fs::write(slots.join("slot-1"), b"").expect("a second slot");
        for s in [15, 30, 69] {
            assert_eq!(reader.read(at(s), now_us(), &CALM, false), free, "at {s} s");
        }
        assert_eq!(reader.read_at(), Some(at(10)));
        let idle = reader.read(at(70), now_us(), &CALM, false);
        assert_eq!(counts_of(&idle).n, 2, "{idle:?}");
        assert_eq!(reader.read_at(), Some(at(70)));
        // Negative control: armed, the same wait re-reads at its period.
        std::fs::write(slots.join("slot-2"), b"").expect("a third slot");
        let armed = reader.read(at(75), now_us(), &CALM, true);
        assert_eq!(counts_of(&armed).n, 3, "{armed:?}");
        assert_eq!(
            default_dir(Path::new("/Users//u")),
            Path::new("/Users//u/.cache/build-slot")
        );
    }

    /// A LOCK HELD WITH NO RECORD TO SHOW IT is never free: a winner that
    /// took the slot's `flock` and stalls before its record (after its
    /// truncate, or before it, over a gone writer's record) reads unknown on
    /// every read, however long it stalls. The lock query is a query: the
    /// file's lock is seen as this test holds it and not once it lets go.
    /// NEGATIVE CONTROL: the lock let go, the same slots pair free.
    #[test]
    fn a_lock_held_with_no_record_to_show_it_is_never_free() {
        let me = std::process::id();
        let born = birth(me).expect("this process's birth");
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let empty = dir.path().join("slot-0");
        let stale = dir.path().join("slot-1");
        std::fs::write(&empty, b"").expect("truncated, no record yet");
        // This process's pid, but a record older than it: a gone writer's.
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
        // Negative control: let go, the same slots read free on two reads.
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

    /// A WRITER WHOSE DESCRIPTORS ARE REFUSED (another user's: a root
    /// daemon's, as `sudo build-slot` would leave) is never read as one that
    /// let go on its descriptors alone: libproc answers the refusal with no
    /// bytes, which is no list, not an empty one. The lock query has the last
    /// word: nobody holds the lock, it let go; someone does, unproven.
    #[test]
    fn a_refused_descriptor_table_is_unproven_until_the_lock_says() {
        // SAFETY: `geteuid` reads this process's effective uid.
        assert_ne!(unsafe { libc::geteuid() }, 0, "run as a user, not root");
        let daemon = kproc::records()
            .expect("the process table")
            .into_iter()
            .find(|p| p.uid == 0 && p.pid > 1 && !p.zombie)
            .expect("a root daemon");
        let table = crate::jobs::macos_vnode_ids(daemon.pid);
        assert_eq!(table, None, "a refused table read as a list");
        // Its record, written after it was born: it is the writer, alive.
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
