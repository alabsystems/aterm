// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-machine oracle tests holding `kproc` process records and renderings against `ps` and `lsof`.

#[cfg(target_os = "macos")]
mod live {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::time::{Duration, Instant};

    use aterm_uds::spawnfd::{Child, Command, Stdio};

    use crate::kproc::*;

    const HANG: Duration = Duration::from_secs(60);

    /// A test binary waits for the real table as the tests' hang detector, never the production
    /// bound a loaded machine can miss.
    #[test]
    fn a_test_binary_waits_for_the_table_as_its_hang_detector() {
        assert!(crate::test_binary::this_is_a_test_binary());
        assert_eq!(
            crate::kproc::imp::table_wait(),
            aterm_spec::wait::MUST_HAPPEN
        );
    }

    fn ps(args: &[&str]) -> Vec<String> {
        ps_in("C", args)
    }

    fn ps_in(locale: &str, args: &[&str]) -> Vec<String> {
        let out = Command::new("/bin/ps")
            .args(args)
            .env("LC_ALL", locale)
            .env("TZ", "UTC")
            .output()
            .expect("run ps");
        assert!(out.status.success(), "ps {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn column(locale: &str, args: &[&str]) -> BTreeMap<u32, String> {
        ps_in(locale, args)
            .into_iter()
            .filter_map(|l| {
                let (pid, rest) = l.trim_start().split_once(' ')?;
                Some((pid.parse().ok()?, rest.to_string()))
            })
            .collect()
    }

    fn lstart_secs(words: &[&str]) -> u64 {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let [_, month, day, clock, year] = words else {
            panic!("lstart {words:?}");
        };
        let m = MONTHS.iter().position(|n| n == month).expect("month") as i64 + 1;
        let (d, y): (i64, i64) = (day.parse().expect("day"), year.parse().expect("year"));
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
        let hms: Vec<i64> = clock
            .split(':')
            .map(|p| p.parse().expect("clock"))
            .collect();
        u64::try_from(days * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2]).expect("after 1970")
    }

    fn by_pid(rows: Vec<Record>) -> BTreeMap<u32, Record> {
        rows.into_iter().map(|r| (r.pid, r)).collect()
    }

    fn until(pid: u32, what: &str, holds: impl Fn(&Record) -> bool) -> Record {
        let start = Instant::now();
        loop {
            if let Some(r) = record(pid).filter(|r| holds(r)) {
                return r;
            }
            assert!(start.elapsed() < HANG, "{what}: not within {HANG:?}");
            aterm_uds::deadline::poll_yield();
        }
    }

    fn zombie() -> Child {
        let child = Command::new("/usr/bin/true").spawn().expect("spawn true");
        let pid = libc::id_t::try_from(child.id()).expect("pid");
        // SAFETY: zeroed `siginfo_t` is a valid out-parameter.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waits for this process's own child without reaping it.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        assert_eq!(rc, 0, "waitid: {}", std::io::Error::last_os_error());
        child
    }

    fn on_a_pty() -> (Child, u32) {
        let script = Command::new("/usr/bin/script")
            .args(["-q", "/dev/null", "/bin/sleep", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn script");
        let me = script.id();
        let start = Instant::now();
        let sleep = loop {
            let found = records()
                .expect("records")
                .into_iter()
                .find(|r| r.ppid == me && r.comm == "sleep");
            if let Some(r) = found {
                break r.pid;
            }
            assert!(
                start.elapsed() < HANG,
                "script's sleep: not within {HANG:?}"
            );
            aterm_uds::deadline::poll_yield();
        };
        (script, sleep)
    }

    fn end(mut c: Child) {
        let _ = c.kill();
        let _ = c.wait();
    }

    /// `ps -o tdev`'s word for a terminal device: `major/minor` of `e_tdev`, `??` for none. `ps`'s
    /// `tty` name comes from `devname(3)`, a scan of `/dev` that skips a live terminal's node while
    /// another process removes an earlier one, so under pty churn it prints `??` for a process that
    /// holds a terminal; `tdev` is the kernel's number, read from the same record kproc reads.
    fn tdev_word(tty: Option<i32>) -> String {
        tty.map_or_else(
            || "??".to_string(),
            |d| {
                let d = d.cast_unsigned();
                format!("{}/{}", (d >> 24) & 0xff, d & 0x00ff_ffff)
            },
        )
    }

    #[test]
    fn every_record_is_the_one_ps_reads() {
        let dead = zombie();
        let (script, pty) = on_a_pty();
        let a = by_pid(records().expect("records"));
        let lines = ps(&[
            "-A",
            "-o",
            "pid=,ppid=,pgid=,tpgid=,uid=,stat=,tdev=,lstart=",
        ]);
        let b = by_pid(records().expect("records"));
        let rows = (dead.id(), pty);
        let live_start = live_start_s(std::process::id());
        let dead_start = live_start_s(dead.id());
        let own_pty = a.get(&pty).and_then(|r| r.tty);
        end(script);
        end(dead);
        assert_eq!(live_start, Some(a[&std::process::id()].start_s));
        assert_eq!(dead_start, None, "an exited process owns no live work");
        let mut compared = BTreeSet::new();
        for line in &lines {
            let w: Vec<&str> = line.split_whitespace().collect();
            let pid: u32 = w[0].parse().expect("pid");
            let (Some(r), Some(again)) = (a.get(&pid), b.get(&pid)) else {
                continue;
            };
            if r != again {
                continue;
            }
            assert_eq!(
                w[1].parse::<u32>().ok(),
                Some(r.ppid),
                "ppid: {line} vs {r:?}"
            );
            assert_eq!(
                w[2].parse::<i32>().ok(),
                Some(r.pgid),
                "pgid: {line} vs {r:?}"
            );
            if r.tty.is_none() || r.tty == own_pty {
                assert_eq!(
                    w[3].parse::<i32>().ok(),
                    Some(r.tpgid),
                    "tpgid: {line} vs {r:?}"
                );
            }
            assert_eq!(
                w[4].parse::<u32>()
                    .ok()
                    .or_else(|| w[4].parse::<i32>().ok().map(i32::cast_unsigned)),
                Some(r.uid),
                "uid: {line} vs {r:?}"
            );
            assert_eq!(w[5].contains('Z'), r.zombie, "zombie: {line} vs {r:?}");
            assert_eq!(w[6], tdev_word(r.tty), "terminal: {line} vs {r:?}");
            assert_eq!(lstart_secs(&w[7..]), r.start_s, "start: {line} vs {r:?}");
            compared.insert(pid);
        }
        for pid in [std::process::id(), rows.0, rows.1] {
            assert!(compared.contains(&pid), "fixture {pid} compared");
        }
        assert!(a[&rows.0].zombie, "the reaped-later child is a zombie");
        assert!(
            a[&rows.1].tty.is_some() && a[&rows.1].tty != a[&std::process::id()].tty,
            "the pty's sleep is on another terminal: {:?}",
            a[&rows.1]
        );
        assert!(
            compared.len() * 2 >= lines.len(),
            "{} of {} rows compared",
            compared.len(),
            lines.len()
        );
    }

    fn every_escape() -> [Vec<Vec<u8>>; 2] {
        let mut words: Vec<Vec<u8>> = [
            "na\u{ef}ve\nline",
            "tab\tx",
            "back\\slash",
            "\u{7f}",
            "\u{2014}",
            "\u{85}",
            "\u{a0}",
            "\u{ad}",
            "\u{200b}",
            "\u{202f}",
            "\u{2028}",
            "\u{3000}",
            "\u{feff}",
            "e\u{301}",
            "\u{378}",
            "\u{e000}",
            "\u{e0001}",
            "\u{1f600}",
        ]
        .iter()
        .map(|w| w.as_bytes().to_vec())
        .collect();
        words.push((1..=0x7f).collect());
        [words, (0x80..=0xffu8).map(|b| vec![b]).collect()]
    }

    fn holding(arg0: &str, words: &[Vec<u8>]) -> Child {
        let mut shell = Command::new("/bin/bash");
        shell.arg0(arg0).args(["-c", "read x"]);
        for w in words {
            shell.arg(OsStr::from_bytes(w));
        }
        shell
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn bash")
    }

    /// Compare stable processes this test owns: its own image, two shells blocked on their
    /// input pipes, and an unreaped zombie. Unrelated processes may exec or rewrite arguments
    /// between the four `ps` calls even when our readings before and after happen to match.
    #[test]
    fn the_comm_and_args_columns_are_the_ones_ps_prints() {
        type Columns = (String, String, String, String);
        let dead = zombie();
        let [printable, raw] = every_escape();
        let mut odd = holding("odd name\u{1}", &printable);
        let mut bytes = holding("raw", &raw);
        until(odd.id(), "the shell's exec", |r| {
            r.comm == "bash" && ps_comm(r, Locale::C) == "odd name^A"
        });
        until(bytes.id(), "the shell's exec", |r| {
            r.comm == "bash" && ps_comm(r, Locale::C) == "raw"
        });
        let fixtures = [std::process::id(), odd.id(), dead.id(), bytes.id()];
        let selected = std::iter::once(1)
            .chain(fixtures)
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let read = || -> BTreeMap<u32, Columns> {
            fixtures
                .into_iter()
                .map(|pid| {
                    let r = record(pid).expect("the fixture is held until the oracle reads it");
                    let columns = (
                        ps_comm(&r, Locale::C),
                        ps_args(&r, Locale::C),
                        ps_comm(&r, Locale::Utf8),
                        ps_args(&r, Locale::Utf8),
                    );
                    (r.pid, columns)
                })
                .collect()
        };
        let a = read();
        let comm = column("C", &["-p", &selected, "-o", "pid=,comm="]);
        let args = column("C", &["-p", &selected, "-ww", "-o", "pid=,args="]);
        let comm_utf8 = column("C.UTF-8", &["-p", &selected, "-o", "pid=,comm="]);
        let args_utf8 = column("C.UTF-8", &["-p", &selected, "-ww", "-o", "pid=,args="]);
        let b = read();
        for shell in [&mut odd, &mut bytes] {
            drop(shell.stdin.take());
            let _ = shell.wait();
        }
        end(dead);
        for pid in fixtures {
            let mine = &a[&pid];
            assert_eq!(b.get(&pid), Some(mine), "fixture {pid} stayed unchanged");
            assert_eq!(Some(&mine.0), comm.get(&pid), "comm of fixture {pid}");
            assert_eq!(Some(&mine.1), args.get(&pid), "args of fixture {pid}");
            assert_eq!(
                Some(&mine.2),
                comm_utf8.get(&pid),
                "comm of fixture {pid}, UTF-8"
            );
            assert_eq!(
                Some(&mine.3),
                args_utf8.get(&pid),
                "args of fixture {pid}, UTF-8"
            );
        }
        let odd_args = &a[&fixtures[1]];
        assert!(
            odd_args.1.starts_with(
                "odd name^A -c read x naM-CM-/ve\\012line tab\\011x back\\slash ^? M-bM^@M^T "
            ),
            "{odd_args:?}"
        );
        assert!(
            odd_args.3.starts_with(
                "odd name^A -c read x na\u{ef}ve\\012line tab\\011x back\\slash ^? \u{2014} "
            ),
            "{odd_args:?}"
        );
        let raw_args = &a[&fixtures[3]].1;
        assert!(
            raw_args.starts_with("raw -c read x M^@ M^A ") && raw_args.ends_with(" M-~ M^?"),
            "{raw_args:?}"
        );
        let defunct = DEFUNCT.to_string();
        let zombie_columns = (defunct.clone(), defunct.clone(), defunct.clone(), defunct);
        assert_eq!(a[&fixtures[2]], zombie_columns);
        let launchd = record(1).expect("launchd");
        let named = (ps_comm(&launchd, Locale::C), ps_args(&launchd, Locale::C));
        assert_eq!(
            (Some(&named.0), Some(&named.1)),
            (comm.get(&1), args.get(&1)),
            "launchd's comm and args"
        );
    }

    /// `lsof` reads only the quiet `sleep` child and launchd, never this process: it reads every
    /// socket and kqueue of a process it lists (`-d txt` narrows only what it prints), and a read
    /// of one a sibling test closes meanwhile drains it.
    #[test]
    fn the_image_file_matches_metadata_and_lsofs_first_txt() {
        use std::os::unix::fs::MetadataExt as _;
        let lsof = |pid: u32| -> Option<u64> {
            let out = Command::new("/usr/sbin/lsof")
                .args([
                    "-n",
                    "-P",
                    "-a",
                    "-p",
                    &pid.to_string(),
                    "-d",
                    "txt",
                    "-F",
                    "i",
                ])
                .output()
                .expect("run lsof");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix('i').and_then(|n| n.parse().ok()))
        };
        let sleep = Command::new("/bin/sleep")
            .arg("120")
            .spawn()
            .expect("sleep");
        let me = std::process::id();
        let exe = std::env::current_exe().expect("exe");
        let meta = std::fs::metadata(exe).expect("exe");
        assert_eq!(
            image_file(me),
            Some(ImageFile {
                device: meta.dev(),
                inode: meta.ino(),
            })
        );
        let system_meta = std::fs::metadata("/bin/sleep").expect("sleep");
        assert_eq!(
            image_file(sleep.id()),
            Some(ImageFile {
                device: system_meta.dev(),
                inode: system_meta.ino(),
            }),
            "the sealed system volume uses the same device convention"
        );
        let slept = image_inode(sleep.id());
        assert_eq!(
            slept,
            Some(std::fs::metadata("/bin/sleep").expect("sleep").ino())
        );
        assert_eq!(slept, lsof(sleep.id()));
        assert_eq!(image_inode(1), lsof(1), "launchd, as this uid reads it");
        end(sleep);
        assert_eq!(image_inode(0), None);
        assert_eq!(image_file(0), None);
    }
}

/// The table's lane inside the exec gate ([`super::imp::inside`]): the lane's own laws (one call
/// on one worker, the stall, contention, a read that panics) are `lane_tests.rs`'s.
#[cfg(target_os = "macos")]
mod gated {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    use super::super::imp::inside;
    use crate::exec_gate::{ExecGate, Reading, StillReading};
    use crate::lane::{Lane, Read};

    /// A bound a read that never ends outlasts: the stall and the refusal under test, not a
    /// latency budget.
    const HELD: Duration = Duration::from_millis(20);

    /// AN EXEC IS REFUSED WHILE THE READ ITS CALLER GAVE UP ON IS STILL IN THE KERNEL, and goes
    /// ahead once that read has returned: the lane names itself stalled and its worker's read
    /// counts in the gate for as long as it is held. A read asked for while the gate is closed is
    /// not made, and its asker is answered nothing at once: that is no stall. The held read
    /// stands for a build front's table read behind another process's exec (m72, 2026-11-12:
    /// seven fronts exec'ed over theirs, and none ever finished).
    /// NEGATIVE CONTROL (by hand: the lane's reader reading inside a gate other than `GATE`, a
    /// fresh `ExecGate`): this gate counts no read.
    #[test]
    fn a_read_its_caller_gave_up_on_refuses_the_exec_until_it_returns() {
        static GATE: ExecGate = ExecGate::new();
        static OPEN: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
        static READS: AtomicUsize = AtomicUsize::new(0);
        /// Held until `OPEN`: the first read stands for one the kernel holds, every later one
        /// for one it answers. Its answer is which read it was.
        fn read(_: &Reading) -> Option<usize> {
            let nth = READS.fetch_add(1, Ordering::SeqCst) + 1;
            let mut open = OPEN.0.lock().unwrap();
            while !*open {
                open = OPEN.1.wait(open).unwrap();
            }
            Some(nth)
        }
        static LANE: Lane<(), Option<usize>> = Lane::new("kproc_table", |()| inside(&GATE, read));
        let Read::Stalled(stall) = LANE.ask_within((), HELD) else {
            panic!("a held read is stalled");
        };
        assert_eq!(stall.lane, "kproc_table");
        until_within(MUST_HAPPEN, "the held read started", || {
            (READS.load(Ordering::SeqCst) == 1).then_some(())
        });
        assert_eq!(GATE.in_kernel(), 1, "one read, counted once");
        assert_eq!(GATE.close(HELD).err(), Some(StillReading(1)));
        assert_eq!(
            LANE.stall().map(|s| s.lane),
            Some("kproc_table"),
            "the read its asker gave up on is still held"
        );
        *OPEN.0.lock().unwrap() = true;
        OPEN.1.notify_all();
        let closed = GATE.close(MUST_HAPPEN).expect("the held read returned");
        until_within(MUST_HAPPEN, "the lane took the late call's return", || {
            LANE.stall().is_none().then_some(())
        });
        match LANE.ask_within((), MUST_HAPPEN) {
            Read::Answered { out, ended, .. } => {
                assert_eq!(out, None, "a closed gate answers nothing");
                assert_eq!(
                    ended.map(|s| s.lane),
                    Some("kproc_table"),
                    "the held read's stall ended"
                );
            }
            other => panic!("a closed gate is an answer, not a stall: {other:?}"),
        }
        assert_eq!(LANE.stall(), None, "a closed gate stalls nothing");
        assert_eq!(
            READS.load(Ordering::SeqCst),
            1,
            "no read behind a closed gate"
        );
        drop(closed);
        assert_eq!(LANE.ask_within((), MUST_HAPPEN).answer(), Some(Some(2)));
        assert_eq!(GATE.in_kernel(), 0, "the answered read left the gate");
    }
}

/// The table's read through its sizing ([`super::imp::table_sized`]) over a stand-in kernel.
#[cfg(target_os = "macos")]
mod sized {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::AtomicUsize;

    use super::super::imp::table_sized;
    use crate::jobs::darwin::KINFO_SIZE;

    /// A stand-in `KERN_PROC_ALL`: a size answer is the table's bytes and xnu's slack of five
    /// records, and a read into a buffer too small for the table writes the records that fit and
    /// answers `ENOMEM`, as xnu's `sysctl_prochandle` does.
    struct Kernel {
        table: RefCell<Vec<u8>>,
        sizes: Cell<usize>,
        reads: Cell<usize>,
    }

    impl Kernel {
        fn holding(records: usize) -> Self {
            Self {
                table: RefCell::new(table(records)),
                sizes: Cell::new(0),
                reads: Cell::new(0),
            }
        }

        fn size(&self) -> Option<usize> {
            self.sizes.set(self.sizes.get() + 1);
            Some(self.table.borrow().len() + 5 * KINFO_SIZE)
        }

        fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            let table = self.table.borrow();
            if table.len() > buf.len() {
                let fit = buf.len() / KINFO_SIZE * KINFO_SIZE;
                buf[..fit].copy_from_slice(&table[..fit]);
                return Err(std::io::Error::from_raw_os_error(libc::ENOMEM));
            }
            buf[..table.len()].copy_from_slice(&table);
            Ok(table.len())
        }

        /// The sizes asked and the reads made since the last take.
        fn take(&self) -> (usize, usize) {
            (self.sizes.take(), self.reads.take())
        }
    }

    /// `records` records, each byte naming its record and offset, so a read of the wrong table
    /// or a short one is told apart.
    fn table(records: usize) -> Vec<u8> {
        (0..records * KINFO_SIZE)
            .map(|i| ((i / KINFO_SIZE) as u8) ^ ((i % 251) as u8))
            .collect()
    }

    /// A TABLE OF A KNOWN SIZE IS READ IN ONE SYSCTL: the first read asks the size and reads
    /// once; every later read asks no size and reads once while the table fits the last read's
    /// bytes and their slack (unchanged, grown a little, shrunk); a table that outgrew them is
    /// refused (`ENOMEM`) and read again at its asked size, three `sysctl`s where a read sized
    /// first makes two. Every answer is the whole table the
    /// stand-in held at that read, as a read sized by its own size answer gives. A size the
    /// kernel does not answer, before any whole read, is no table.
    /// NEGATIVE CONTROL: a read that asks the size before every read (the sizing before this
    /// cut) asks one at each of the later reads here.
    #[test]
    fn a_table_of_a_known_size_is_read_with_no_size_asked() {
        let last = AtomicUsize::new(0);
        let kernel = Kernel::holding(700);
        let read = || table_sized(&last, || kernel.size(), |buf| kernel.read(buf));
        assert_eq!(read(), Some(table(700)));
        assert_eq!(kernel.take(), (1, 1), "the first read asks the size");
        assert_eq!(read(), Some(table(700)));
        assert_eq!(kernel.take(), (0, 1), "an unchanged table: no size asked");
        *kernel.table.borrow_mut() = table(760);
        assert_eq!(read(), Some(table(760)));
        assert_eq!(
            kernel.take(),
            (0, 1),
            "grown within the slack: no size asked"
        );
        *kernel.table.borrow_mut() = table(1_000);
        assert_eq!(read(), Some(table(1_000)));
        assert_eq!(
            kernel.take(),
            (1, 2),
            "outgrown: refused, then read at its asked size"
        );
        *kernel.table.borrow_mut() = table(300);
        assert_eq!(read(), Some(table(300)));
        assert_eq!(kernel.take(), (0, 1), "shrunk: no size asked");

        let never = AtomicUsize::new(0);
        assert_eq!(table_sized(&never, || None, |buf| kernel.read(buf)), None);
        assert_eq!(kernel.take(), (0, 0), "no size answer, no read");
    }
}

/// LS-15: A NAME NOT READ IS NO NAME. Where the kernel reads a process's arguments and its
/// `KERN_PROCARGS2` read fails (a loaded host, a process mid-exec), `ps` prints the placeholder
/// `(comm)`: [`crate::kproc::comm_read`] answers none there, so no caller takes `(aterm)` for a
/// program that is not aterm. CONTROLS: the same record with its arguments read is named by its
/// `argv[0]`, another user's (whose arguments are never read) by what `ps` prints for it, and a
/// zombie by `<defunct>`, as [`crate::kproc::ps_comm`] names each.
#[cfg(target_os = "macos")]
#[test]
fn a_name_whose_arguments_were_not_read_is_not_read() {
    use crate::kproc::{DEFUNCT, Locale, Record, comm_read_by};
    // SAFETY: `geteuid` reads this process's effective uid.
    let me = unsafe { libc::geteuid() };
    let record = |uid: u32, zombie: bool| Record {
        pid: 4_242,
        ppid: 1,
        pgid: 4_242,
        tpgid: 0,
        tty: None,
        uid,
        comm: "aterm".into(),
        start_s: 0,
        start_us: 0,
        zombie,
    };
    let mine = record(me, false);
    assert_eq!(comm_read_by(&mine, Locale::C, |_| None), None);
    assert_eq!(
        crate::kproc::unread(&mine, Locale::C),
        "(aterm)",
        "what ps prints for it is the placeholder, never a read name"
    );
    let words = |_| {
        Some(vec![
            b"/Applications/aterm.app/Contents/MacOS/aterm".to_vec(),
        ])
    };
    assert_eq!(
        comm_read_by(&mine, Locale::C, words).as_deref(),
        Some("/Applications/aterm.app/Contents/MacOS/aterm")
    );
    assert_eq!(
        comm_read_by(&record(me, true), Locale::C, |_| None).as_deref(),
        Some(DEFUNCT)
    );
    if me != 0 {
        let theirs = record(me.wrapping_add(1), false);
        assert!(
            comm_read_by(&theirs, Locale::C, |_| None).is_some(),
            "another user's arguments are never asked: ps names it as it can"
        );
    }
}
