// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kernel's process records, held to `ps` and `lsof` (their oracles, run
//! here only) on the live machine, and the pure renderings to the ones those
//! tools were measured to give. `KERN_PROCARGS2`'s layout is
//! `aterm_uds::procargs`'s, tested there.

#[cfg(target_os = "macos")]
mod live {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::time::{Duration, Instant};

    use aterm_uds::spawnfd::{Child, Command, Stdio};

    use crate::kproc::*;

    /// The hang detector for what must happen (a child's exec, its exit).
    const HANG: Duration = Duration::from_secs(60);

    /// `/bin/ps` in the C locale and UTC: its lines.
    fn ps(args: &[&str]) -> Vec<String> {
        ps_in("C", args)
    }

    /// `/bin/ps` under `LC_ALL=<locale>` and UTC: its lines (read as the
    /// readers read theirs, a byte that is no UTF-8 replaced).
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

    /// `ps -o pid=,<one column>=`'s rows under `LC_ALL=<locale>`: the pid,
    /// then the rest of the line after its one separating space, byte for
    /// byte.
    fn column(locale: &str, args: &[&str]) -> BTreeMap<u32, String> {
        ps_in(locale, args)
            .into_iter()
            .filter_map(|l| {
                let (pid, rest) = l.trim_start().split_once(' ')?;
                Some((pid.parse().ok()?, rest.to_string()))
            })
            .collect()
    }

    /// `ps -o lstart=` (C locale, UTC: `Tue Oct 20 02:01:00 2026`) in unix
    /// seconds.
    fn lstart_secs(words: &[&str]) -> u64 {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let [_, month, day, clock, year] = words else {
            panic!("lstart {words:?}");
        };
        let m = MONTHS.iter().position(|n| n == month).expect("month") as i64 + 1;
        let (d, y): (i64, i64) = (day.parse().expect("day"), year.parse().expect("year"));
        // Days from the civil date (Howard Hinnant's algorithm).
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

    /// Wait (a hang detector, never a budget) until `pid`'s record holds.
    fn until(pid: u32, what: &str, holds: impl Fn(&Record) -> bool) -> Record {
        let start = Instant::now();
        loop {
            if let Some(r) = record(pid).filter(|r| holds(r)) {
                return r;
            }
            assert!(start.elapsed() < HANG, "{what}: not within {HANG:?}");
            std::thread::yield_now();
        }
    }

    /// A child that has exited and is not reaped: `waitid(WNOWAIT)` returns
    /// once it exits and leaves it a zombie.
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

    /// A `sleep` on a pty `script` opened: a process whose terminal is not
    /// this test's.
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
            std::thread::yield_now();
        };
        (script, sleep)
    }

    fn end(mut c: Child) {
        let _ = c.kill();
        let _ = c.wait();
    }

    /// EVERY RECORD IS THE ONE `ps` READS, for every user's process: parent,
    /// group, terminal, uid, start time and whether it is a zombie, and the
    /// terminal's foreground group wherever no other program can move it
    /// inside the window (no terminal, or the pty this test holds). A process
    /// that changed between the two reads that sandwich `ps` is not compared;
    /// most are, and the fixtures always are (this process, a zombie, a
    /// process on another terminal), so the check can never pass having
    /// compared nothing. `ps` prints a terminal's name and a record its
    /// device: they agree when each name stands for exactly one device and
    /// `??` for none.
    #[test]
    fn every_record_is_the_one_ps_reads() {
        let dead = zombie();
        let (script, pty) = on_a_pty();
        let a = by_pid(records().expect("records"));
        let lines = ps(&[
            "-A",
            "-o",
            "pid=,ppid=,pgid=,tpgid=,uid=,stat=,tty=,lstart=",
        ]);
        let b = by_pid(records().expect("records"));
        let rows = (dead.id(), pty);
        let own_pty = a.get(&pty).and_then(|r| r.tty);
        end(script);
        end(dead);
        let mut compared = BTreeSet::new();
        let mut names: BTreeMap<String, BTreeSet<Option<i32>>> = BTreeMap::new();
        let mut devices: BTreeMap<Option<i32>, BTreeSet<String>> = BTreeMap::new();
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
            // A terminal's foreground group is moved by whoever runs a job on
            // it, and a short job is gone again before the second read (the
            // group back where it was, the record equal), so it is compared
            // where nothing else moves it: no terminal, or this test's pty.
            if r.tty.is_none() || r.tty == own_pty {
                assert_eq!(
                    w[3].parse::<i32>().ok(),
                    Some(r.tpgid),
                    "tpgid: {line} vs {r:?}"
                );
            }
            // `ps` prints a uid as a signed number: `nobody` (4294967294,
            // macOS's dhcp6d runs as it) reads `-2`.
            assert_eq!(
                w[4].parse::<u32>()
                    .ok()
                    .or_else(|| w[4].parse::<i32>().ok().map(i32::cast_unsigned)),
                Some(r.uid),
                "uid: {line} vs {r:?}"
            );
            assert_eq!(w[5].contains('Z'), r.zombie, "zombie: {line} vs {r:?}");
            assert_eq!(
                w[6] == "??",
                r.tty.is_none(),
                "no terminal: {line} vs {r:?}"
            );
            assert_eq!(lstart_secs(&w[7..]), r.start_s, "start: {line} vs {r:?}");
            names.entry(w[6].to_string()).or_default().insert(r.tty);
            devices.entry(r.tty).or_default().insert(w[6].to_string());
            compared.insert(pid);
        }
        for (name, devs) in &names {
            assert_eq!(devs.len(), 1, "`{name}` names one device: {devs:?}");
        }
        for (dev, ttys) in &devices {
            assert_eq!(ttys.len(), 1, "{dev:?} has one name: {ttys:?}");
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

    /// The fixtures' words after `bash -c 'read x'`, every escape `ps` makes
    /// in each locale. The first: characters beyond ASCII that print and
    /// that do not (a UTF-8 locale keeps the one and escapes the other's code
    /// point), then every ASCII control and printable byte. The second: every
    /// high byte alone, no UTF-8, so a UTF-8 locale writes its whole string
    /// out byte by byte while the C locale escapes each.
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

    /// A `bash` blocked in `read`, named `arg0`, holding `words`.
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

    /// A live process (no zombie) whose `KERN_PROCARGS2` record the kernel will not hand
    /// over whole: [`argv`] reads it as unread, where `ps` prints the record's tail.
    fn past_what_the_kernel_hands_over(pid: u32) -> bool {
        crate::procargs::read(pid).is_none() && record(pid).is_some_and(|r| !r.zombie)
    }

    /// `ps`'s `comm` and `args` columns, in the C locale and in a UTF-8 one,
    /// for every process of this uid: `argv[0]` and the argument words as
    /// `ps` escapes them, a zombie's `<defunct>`. Another user's process is
    /// `ps`'s (setuid root) alone to read, and is not compared. The fixture's
    /// `argv[0]` is no program's name and its words hold every escape
    /// ([`every_escape`]). The one row that may differ is a live process whose
    /// record is past what the kernel hands over ([`argv`]: `ps` prints its
    /// tail as words), such as aterm-uds's `procargs` test's own stage.
    #[test]
    fn the_comm_and_args_columns_are_the_ones_ps_prints() {
        type Columns = (String, String, String, String);
        let dead = zombie();
        // `bash` itself: macOS's `/bin/sh` execs another shell in place, and
        // a read across that exec would see neither.
        let [printable, raw] = every_escape();
        let mut odd = holding("odd name\u{1}", &printable);
        let mut bytes = holding("raw", &raw);
        until(odd.id(), "the shell's exec", |r| {
            r.comm == "bash" && ps_comm(r, Locale::C) == "odd name^A"
        });
        until(bytes.id(), "the shell's exec", |r| {
            r.comm == "bash" && ps_comm(r, Locale::C) == "raw"
        });
        // SAFETY: `geteuid` reads this process's effective uid.
        let me = unsafe { libc::geteuid() };
        let read = || -> BTreeMap<u32, Columns> {
            records()
                .expect("records")
                .iter()
                .filter(|r| r.uid == me)
                .map(|r| {
                    let columns = (
                        ps_comm(r, Locale::C),
                        ps_args(r, Locale::C),
                        ps_comm(r, Locale::Utf8),
                        ps_args(r, Locale::Utf8),
                    );
                    (r.pid, columns)
                })
                .collect()
        };
        let a = read();
        let comm = column("C", &["-A", "-o", "pid=,comm="]);
        let args = column("C", &["-A", "-ww", "-o", "pid=,args="]);
        let comm_utf8 = column("C.UTF-8", &["-A", "-o", "pid=,comm="]);
        let args_utf8 = column("C.UTF-8", &["-A", "-ww", "-o", "pid=,args="]);
        let b = read();
        let fixtures = [std::process::id(), odd.id(), dead.id(), bytes.id()];
        for shell in [&mut odd, &mut bytes] {
            drop(shell.stdin.take());
            let _ = shell.wait();
        }
        end(dead);
        let mut compared = BTreeSet::new();
        for (pid, mine) in &a {
            if b.get(pid) != Some(mine) {
                continue;
            }
            let oracle = (
                comm.get(pid),
                args.get(pid),
                comm_utf8.get(pid),
                args_utf8.get(pid),
            );
            let (Some(c), Some(g), Some(cu), Some(gu)) = oracle else {
                continue;
            };
            let differs = (&mine.0, &mine.1, &mine.2, &mine.3) != (c, g, cu, gu);
            if differs && past_what_the_kernel_hands_over(*pid) {
                continue;
            }
            assert_eq!(&mine.0, c, "comm of {pid}");
            assert_eq!(&mine.1, g, "args of {pid}");
            assert_eq!(&mine.2, cu, "comm of {pid}, UTF-8");
            assert_eq!(&mine.3, gu, "args of {pid}, UTF-8");
            compared.insert(*pid);
        }
        for pid in fixtures {
            assert!(
                compared.contains(&pid),
                "fixture {pid} compared: {:?} {:?} {:?} {:?}",
                a.get(&pid),
                b.get(&pid),
                comm.get(&pid),
                args.get(&pid)
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
        // Another user's process (launchd, root's): its words are `ps`'s
        // alone (setuid root), and it is named by its image's path, which
        // `ps` prints as `comm` where `argv[0]` is that path, as launchd's is.
        let launchd = record(1).expect("launchd");
        let named = (ps_comm(&launchd, Locale::C), ps_args(&launchd, Locale::C));
        assert_eq!(
            (Some(&named.0), Some(&named.1)),
            (comm.get(&1), args.get(&1)),
            "launchd's comm and args"
        );
        assert!(
            compared.len() * 2 >= a.len(),
            "{} of {} rows compared",
            compared.len(),
            a.len()
        );
    }

    /// `lsof -d txt`'s first inode, as this uid reads it: this process's
    /// image, a child's, and none for a process it may not read (launchd).
    #[test]
    fn the_image_inode_is_lsofs_first_txt() {
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
        assert_eq!(
            image_inode(me),
            Some(std::fs::metadata(exe).expect("exe").ino())
        );
        assert_eq!(image_inode(me), lsof(me));
        let slept = image_inode(sleep.id());
        assert_eq!(
            slept,
            Some(std::fs::metadata("/bin/sleep").expect("sleep").ino())
        );
        assert_eq!(slept, lsof(sleep.id()));
        assert_eq!(image_inode(1), lsof(1), "launchd, as this uid reads it");
        end(sleep);
        assert_eq!(image_inode(0), None);
    }
}
