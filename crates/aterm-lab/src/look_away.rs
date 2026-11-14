// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `look-away` ([`cmd_look_away`]): a look from another app is no look (§14.11 of
//! `docs/DESIGN-harness-autopilot-2026-10-15.md`), read on a REAL WINDOW. A private windowed
//! aterm runs as its own launchd job, as aterm.app does, so Launch Services knows it and macOS can
//! make it the active app. (A lab's jailed window never can: the jail denies `launchservicesd`,
//! so its aterm never checks in and its window is never key.) Its front tab's task is marked
//! working, then done, while another app the drive opens itself is the active app: the done
//! must stand unseen, and must still stand after a controller's `focus in` and a second done (a
//! ctl client's focus report is no look). Once that app is quit by its PID and aterm is the
//! active app again, the done must be seen, and a done while aterm stays active must be seen as
//! it lands, even after a controller's `focus out`. `--aterm` picks the aterm under test, so the
//! same verb reads main's build as the negative control (there the done is seen while they are
//! away), and a build whose look reads the controller-written focus flag fails the two
//! controller legs.
//!
//! A done nobody saw is the tab's `•`, which the front tab never wears, so each read asks the
//! session itself: `status done_unseen=`, a development build's field that states the rule the
//! `•` reads for a tab in front too ([`Drive::unseen`]). A read moves no tab: bringing the tab
//! under test back in front while aterm is the active app is itself a look, and would make the
//! done seen whatever their return did. A build with no such field (a release build, one from
//! before the field) reads `?`, which is COULD NOT RUN.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::band::Band;
use crate::instance::{self, CONFIG_OFF, Instance, Launch, World};
use crate::{Fail, Res, stop};

/// The app the drive makes active in aterm's place: a small window that covers no aterm window
/// whole, no documents to save, and nothing it restores (`open -n -F`).
const OTHER_APP: &str = "Calculator";
/// Its executable, by which its new process is told from any other.
const OTHER_EXE: &str = "/System/Applications/Calculator.app/Contents/MacOS/Calculator";

/// A hang detector for an activation, a process start or a look: a minute.
const HANG: Duration = Duration::from_secs(60);

const USAGE: &str = "usage: targo --unverified lab look-away [--aterm PATH] [--keep]

OPENS A WINDOW on this desk (take the desk's window lock first). A private aterm runs as its own
launchd job; its front tab's task is marked done while Calculator, opened and quit by PID here,
is the active app. PASS: the done stands unseen while Calculator is active (`status
done_unseen=1`, a development build's field; no tab moves for a read), still stands after
`ctl focus in` and a second done (unseen=1), is seen once aterm is active again (unseen=0), and
a done while aterm stays active is seen as it lands, even after `ctl focus out` (unseen=0).
--aterm picks a build to read as the negative control (unseen=0 away); one with no
`done_unseen=` reads `?` and could not run.
Exit 0 PASS, 1 FAIL, 3 COULD NOT RUN.
";

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("look-away: COULD NOT RUN: {m}"))
}

/// The other app's process, quit by its PID when the drive ends however it ends.
struct OtherApp {
    pid: i32,
}

/// Whether `pid` is gone within `limit`, polling `kill(pid, 0)`. Not this lab's child, so no
/// exit watch; and no [`crate::wait_until`], whose wait ends at once on a latched interrupt, so
/// the quit waits its full time on an interrupted run too (as its `Drop` does).
fn gone_within(pid: i32, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    loop {
        if !crate::procs::alive(pid) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl OtherApp {
    /// Quit it by its PID (`SIGTERM`, then `SIGKILL`), once: whether it is gone.
    fn quit(&mut self) -> bool {
        if self.pid == 0 {
            return true;
        }
        let pid = self.pid;
        let _ = crate::sys::kill(pid, libc::SIGTERM);
        let gone = gone_within(pid, Duration::from_secs(10)) || {
            let _ = crate::sys::kill(pid, libc::SIGKILL);
            gone_within(pid, Duration::from_secs(10))
        };
        println!(
            "  quit      {OTHER_APP} pid {pid}: {}{}",
            if gone { "gone" } else { "STILL ALIVE" },
            interrupt_words()
        );
        self.pid = 0;
        gone
    }
}

/// `; interrupted by signal N` once an interrupt is latched, else nothing.
fn interrupt_words() -> String {
    stop::check()
        .err()
        .map_or_else(String::new, |s| format!("; interrupted by signal {}", s.0))
}

impl Drop for OtherApp {
    fn drop(&mut self) {
        let _ = self.quit();
    }
}

/// Every PID whose command line runs [`OTHER_EXE`]: one read of the process table, and the
/// command line read ([`crate::procs::command_line`]) only of a process the kernel names by
/// [`OTHER_EXE`]'s file name.
fn other_pids() -> Vec<i32> {
    let name = std::path::Path::new(OTHER_EXE)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(OTHER_APP);
    aterm_sysprobe::kproc::records()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.comm == name)
        .filter_map(|r| i32::try_from(r.pid).ok())
        .filter(|&pid| crate::procs::command_line(pid).starts_with(OTHER_EXE))
        .collect()
}

/// [`OTHER_APP`] opened as a new process of its own (`open -n -F -a`), which Launch Services
/// makes the active app.
fn open_other() -> Result<OtherApp, Fail> {
    let before = other_pids();
    let out = aterm_uds::spawnfd::command("/usr/bin/open")
        .args(["-n", "-F", "-a", OTHER_APP])
        .output()
        .map_err(|e| could_not(&format!("open -a {OTHER_APP}: {e}")))?;
    if !out.status.success() {
        return Err(could_not(&format!(
            "open -a {OTHER_APP}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut pid = None;
    let started = crate::wait_until(HANG, || {
        pid = other_pids().into_iter().find(|p| !before.contains(p));
        pid.is_some()
    });
    pid.filter(|_| started)
        .map(|pid| OtherApp { pid })
        .ok_or_else(|| could_not(&format!("no new {OTHER_APP} process within a minute")))
}

/// The PID on an `lsappinfo info -only pid` reply (`… pid = 4242 …`).
#[must_use]
pub fn pid_of_info(info: &str) -> Option<i32> {
    let (_, after) = info.split_once("pid = ")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The PID of the active app, as Launch Services says (`lsappinfo front`, then its `pid`): no
/// privacy consent, no WindowServer connection of this process.
fn front_pid() -> Option<i32> {
    let run = |args: &[&str]| {
        aterm_uds::spawnfd::command("/usr/bin/lsappinfo")
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let asn = run(&["front"]).filter(|a| a.starts_with("ASN:"))?;
    pid_of_info(&run(&["info", "-only", "pid", &asn])?)
}

/// The front tab's session: the `s-…` word of the `ls` row whose tab is in front (`active=1`).
#[must_use]
pub fn front_sid(ls: &str) -> Option<String> {
    ls.lines()
        .filter(|l| l.split_whitespace().any(|w| w == "active=1"))
        .find_map(|l| l.split_whitespace().find(|w| w.starts_with("s-")))
        .map(str::to_string)
}

/// `done_unseen=` on a session's `status` reply: `1` while its done stands unseen, `0` once it
/// was looked at. `None` where the reply is no `OK` line or names no such field.
#[must_use]
pub fn unseen_of(status: &str) -> Option<&str> {
    status
        .strip_prefix("OK ")?
        .split_whitespace()
        .find_map(|w| w.strip_prefix("done_unseen="))
        .filter(|v| matches!(*v, "0" | "1"))
}

/// The four reads of a drive (`unseen=` each).
#[derive(Clone, Copy, Debug)]
pub struct Reads<'a> {
    /// A done while another app is active.
    pub away: &'a str,
    /// Another done after a controller's `focus in`, the other app still active.
    pub faked: &'a str,
    /// Once aterm is the active app again.
    pub back: &'a str,
    /// A done while aterm is active, after a controller's `focus out`.
    pub present: &'a str,
}

/// The verdict on the reads: away the done stands unseen and a controller's `focus in` does not
/// see it, back it is seen, and a done while aterm is active is seen as it lands whatever a
/// controller's `focus out` said.
#[must_use]
pub fn verdict(r: Reads<'_>) -> (bool, String) {
    let said = format!(
        "away unseen={}, want 1; after ctl focus in unseen={}, want 1; back unseen={}, want 0; a done while aterm is active after ctl focus out unseen={}, want 0",
        r.away, r.faked, r.back, r.present
    );
    (
        r.away == "1" && r.faked == "1" && r.back == "0" && r.present == "0",
        said,
    )
}

/// The drive on a booted instance.
struct Drive {
    inst: Instance,
    aterm_pid: i32,
    sid: String,
    t0: Instant,
}

impl Drive {
    fn ctl(&self, args: &[&str]) -> String {
        self.inst.ctl(args).stdout
    }

    /// Whether the tab under test's done stands unseen, as its own `status` says
    /// ([`unseen_of`]): a read that moves no tab and looks at nothing. `?` where the reply
    /// names no `done_unseen=`.
    fn unseen(&self) -> String {
        let status = self.ctl(&[&format!("@{}", self.sid), "status"]);
        unseen_of(&status).unwrap_or("?").to_string()
    }

    /// Mark the front tab's task working, then done; each reply must say so.
    fn mark_done(&self) -> Result<String, Fail> {
        let at = format!("@{}", self.sid);
        for state in ["working", "done"] {
            let said = self.ctl(&[&at, "task", "mark", state]);
            if !said.starts_with("OK task ") || !said.contains(&format!(" state={state} ")) {
                return Err(could_not(&format!(
                    "task mark {state} on {at} said {:?}",
                    said.trim()
                )));
            }
        }
        Ok(format!("{at} task marked working, then done"))
    }

    /// A controller's focus report (`@sid focus in|out`): its reply must be `OK`.
    fn ctl_focus(&self, word: &str) -> Result<(), Fail> {
        let at = format!("@{}", self.sid);
        let said = self.ctl(&[&at, "focus", word]);
        if said.trim() == "OK" {
            return Ok(());
        }
        Err(could_not(&format!(
            "focus {word} on {at} said {:?}",
            said.trim()
        )))
    }

    /// Wait until the active app is `pid`.
    fn await_front(&self, pid: i32, what: &str) -> Result<(), Fail> {
        if crate::wait_until(HANG, || front_pid() == Some(pid)) {
            return Ok(());
        }
        stop::check()?;
        Err(could_not(&format!(
            "{what} (pid {pid}) was not the active app within a minute (front pid {:?})",
            front_pid()
        )))
    }

    fn at(&self) -> String {
        format!("+{:.1} s", self.t0.elapsed().as_secs_f64())
    }

    fn run(&self) -> Res {
        let raised = self.ctl(&["raise", &self.sid]);
        self.await_front(self.aterm_pid, "aterm after raise")?;
        let before = self.unseen();
        if before != "0" {
            return Err(could_not(&format!(
                "the tab under test read unseen={before} before the drive"
            )));
        }
        println!(
            "  front     {} raise @{}: {}; aterm pid {} is the active app; unseen={before}",
            self.at(),
            self.sid,
            raised.trim(),
            self.aterm_pid
        );
        let mut other = open_other()?;
        self.await_front(other.pid, OTHER_APP)?;
        println!(
            "  away      {} {OTHER_APP} pid {} is the active app",
            self.at(),
            other.pid
        );
        let marked = self.mark_done()?;
        let away = self.unseen();
        let away_front = front_pid();
        println!(
            "  end       {} {marked}; unseen={away} (front pid {away_front:?})",
            self.at()
        );
        if away_front != Some(other.pid) {
            return Err(could_not(&format!(
                "{OTHER_APP} lost the front before the read (front pid {away_front:?})"
            )));
        }
        self.ctl_focus("in")?;
        let marked = self.mark_done()?;
        let faked = self.unseen();
        let faked_front = front_pid();
        println!(
            "  faked     {} ctl focus in, then {marked}; unseen={faked} (front pid {faked_front:?})",
            self.at()
        );
        if faked_front != Some(other.pid) {
            return Err(could_not(&format!(
                "{OTHER_APP} lost the front before the read (front pid {faked_front:?})"
            )));
        }
        self.ctl_focus("out")?;
        if !other.quit() {
            stop::check()?;
            return Err(could_not(&format!(
                "{OTHER_APP} outlived SIGTERM and SIGKILL by its PID for 20 s"
            )));
        }
        let raised = self.ctl(&["raise", &self.sid]);
        self.await_front(self.aterm_pid, "aterm after its return")?;
        let mut back = self.unseen();
        let gone = crate::wait_until(HANG, || {
            back = self.unseen();
            back == "0"
        });
        println!(
            "  back      {} raise @{}: {}; aterm is the active app; unseen={back}{}",
            self.at(),
            self.sid,
            raised.trim(),
            if gone {
                ""
            } else {
                " (the done stood unseen a minute)"
            }
        );
        self.ctl_focus("out")?;
        let marked = self.mark_done()?;
        let present = self.unseen();
        println!(
            "  present   {} ctl focus out, then {marked} while aterm is active; unseen={present} (front pid {:?})",
            self.at(),
            front_pid()
        );
        stop::check()?;
        let (pass, said) = verdict(Reads {
            away: &away,
            faked: &faked,
            back: &back,
            present: &present,
        });
        println!(
            "look-away: {} {}: {said}",
            if pass { "PASS" } else { "FAIL" },
            self.inst.aterm.display()
        );
        Ok(i32::from(!pass))
    }
}

fn parse_args(args: &[String]) -> Result<Option<(Option<String>, bool)>, Fail> {
    let usage = |m: &str| Fail::usage(format!("look-away: {m}\n{USAGE}"));
    let mut a = args.to_vec();
    if instance::take_flag(&mut a, "-h") || instance::take_flag(&mut a, "--help") {
        return Ok(None);
    }
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let keep = instance::take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(usage(&format!("unexpected '{extra}'")));
    }
    Ok(Some((aterm, keep)))
}

/// `look-away [--aterm PATH] [--keep]` ([`USAGE`]).
pub fn cmd_look_away(args: &[String]) -> Res {
    let Some((aterm, keep)) = parse_args(args)? else {
        print!("{USAGE}");
        return Ok(0);
    };
    stop::install();
    let aterm: PathBuf = instance::resolve_aterm(aterm).map_err(|e| could_not(&e))?;
    let mut world = World::new(CONFIG_OFF).map_err(|e| could_not(&e))?;
    if keep {
        world.keep();
    }
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: false,
            args: Vec::new(),
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        Band::App,
    )
    .map_err(|e| could_not(&e))?;
    println!(
        "look-away: {} OPENED A WINDOW on this desk (its own launchd job, keeper pid {})",
        aterm.display(),
        inst.pid
    );
    inst.await_socket(instance::BOOT)?;
    let mut aterm_pid = None;
    let up = crate::wait_until(HANG, || {
        aterm_pid = inst.aterm_pid();
        aterm_pid.is_some()
    });
    let Some(aterm_pid) = aterm_pid.filter(|_| up) else {
        return Err(could_not("the keeper started no aterm within a minute"));
    };
    let ls = inst.ctl(&["ls"]).stdout;
    let Some(sid) = front_sid(&ls) else {
        return Err(could_not(&format!("no tab in front: {}", ls.trim())));
    };
    let drive = Drive {
        inst,
        aterm_pid,
        sid,
        t0: Instant::now(),
    };
    let idle_at_start = crate::meta_keys::hid_idle();
    let verdict = drive.run();
    let started = drive.t0.elapsed();
    drop(drive);
    if verdict.is_ok() && crate::meta_keys::hid_idle().is_none_or(|idle| idle < started) {
        return Err(could_not(&format!(
            "a person touched the keyboard or pointer during the drive (idle at its start {idle_at_start:?}), so the active app may have been theirs to choose"
        )));
    }
    verdict
}

#[cfg(test)]
#[path = "look_away_tests.rs"]
mod tests;
