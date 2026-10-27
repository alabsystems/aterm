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
            std::thread::yield_now();
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
            std::thread::yield_now();
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
        let own_pty = a.get(&pty).and_then(|r| r.tty);
        end(script);
        end(dead);
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

    fn past_what_the_kernel_hands_over(pid: u32) -> bool {
        crate::procargs::read(pid).is_none() && record(pid).is_some_and(|r| !r.zombie)
    }

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

    /// `lsof` reads only the quiet `sleep` child and launchd, never this process: it reads every
    /// socket and kqueue of a process it lists (`-d txt` narrows only what it prints), and a read
    /// of one a sibling test closes meanwhile drains it.
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
