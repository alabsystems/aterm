// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `selftest [tag] [--keep]`: end-to-end lab self-test driving Claude Code, [`crate::settings_witness`], and jail/stray checks.
use std::time::Duration;

use crate::LAB;
use crate::codex_proof::Verdict;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::run::Run;
use crate::{Fail, Res, stop};

/// Runs `selftest [tag] [--keep]`.
pub fn cmd_selftest(args: &[String]) -> Res {
    let (mut tag, mut keep) = ("selftest".to_string(), false);
    for a in args {
        if a == "--keep" {
            keep = true;
        } else {
            tag.clone_from(a);
        }
    }
    if !crate::paths::valid_tag(&tag) {
        return Err(Fail::die(format!("bad tag '{tag}'")));
    }
    stop::install();
    let o = UpOpts {
        lifeline: !keep,
        ..UpOpts::from_env()
    };
    let run = crate::lab::up(&tag, &o).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            Fail::die(format!(
                "selftest: up failed: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    let result = checks(&run);
    let (mut n, mut bad) = (0, 0);
    match (&result, keep) {
        (Ok(_), true) => println!("selftest: lab '{tag}' left up ({LAB} down {tag})"),
        (Ok(c), false) => {
            let k = crate::lab::down(&tag, false)?;
            println!("lab '{tag}' is down; logs kept in {}", k.display());
            let left = jail_left(&run.mark, &c.before);
            n += 1;
            let label = format!(
                "nothing in the lab's jail is alive after down, asked of the kernel by its mark (before it: {} \
                 processes, the orphan among them){}",
                c.before.len(),
                if left.is_empty() {
                    String::new()
                } else {
                    format!(": alive {left}")
                }
            );
            if !c.before.is_empty() && left.is_empty() {
                println!("  ok   {label}");
            } else {
                bad += 1;
                println!("  FAIL {label}");
            }
            let tapes = crate::tape::replay_all(&k.join("drive").join("tapes"));
            let failed: Vec<String> = tapes
                .iter()
                .filter(|(_, v)| {
                    !matches!(
                        v,
                        Ok(aterm_agent::supervise::tape::report::Verdict::Reproduced)
                    )
                })
                .map(|(p, v)| match v {
                    Ok(v) => format!("{} {}", p.display(), v.word()),
                    Err(e) => format!("{} {e}", p.display()),
                })
                .collect();
            n += 1;
            let label = format!(
                "the harness recorded its loops to {} tape(s), kept with the logs, and each replays \
                 from itself alone{}",
                tapes.len(),
                if failed.is_empty() {
                    String::new()
                } else {
                    format!(": {}", failed.join("; "))
                }
            );
            if !tapes.is_empty() && failed.is_empty() {
                println!("  ok   {label}");
            } else {
                bad += 1;
                println!("  FAIL {label}");
            }
        }
        (Err(_), _) => {
            let _ = crate::lab::down(&tag, false);
        }
    }
    let c = result?;
    let (n, bad) = (n + c.n, bad + c.bad);
    if bad == 0 {
        println!("selftest: PASS ({n} checks)");
        Ok(0)
    } else {
        println!("selftest: FAIL ({bad} of {n} checks)");
        Ok(1)
    }
}

/// Summary of selftest check counts and pre-teardown jail PIDs.
pub struct Checks {
    /// Number of checks executed.
    pub n: u32,
    /// Number of failed checks.
    pub bad: u32,
    /// Jail member PIDs recorded before teardown.
    pub before: Vec<i32>,
}

/// Spawns a jailed orphan control process adopted by `launchd` and returns its PID.
#[must_use]
pub fn jail_orphan(run: &Run) -> i32 {
    let bin = run.run.join("bin/aterm-lab").to_string_lossy().into_owned();
    crate::procs::jailed(
        run,
        &run.w,
        &[],
        &[
            "/bin/sh",
            "-c",
            "nohup \"$1\" hold \"$2\" 120 >/dev/null 2>&1 & echo $!",
            "sh",
            &bin,
            &run.port,
        ],
        crate::procs::JailOut::Capture,
    )
    .ok()
    .and_then(|(_, out)| out.trim().parse().ok())
    .unwrap_or(0)
}

/// Whether the witness logged `pid`'s connection to the stub (`cmd[pid] TCP conn 127.0.0.1:<port>`).
fn witnessed(run: &Run, pid: i32) -> bool {
    let want = format!("[{pid}] TCP conn 127.0.0.1:{}", run.port);
    crate::util::read_regular_text(&run.run.join("sockets.log"))
        .unwrap_or_default()
        .lines()
        .any(|l| l.ends_with(&want))
}

/// Returns whether `pid` was adopted by `launchd`, carries the jail mark, and was logged by the egress witness.
#[must_use]
pub fn orphan_seen(run: &Run, pid: i32) -> bool {
    for _ in 0..20 {
        if witnessed(run, pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let ppid = crate::procs::table()
        .into_iter()
        .find(|&(p, _)| p == pid)
        .map(|(_, pp)| pp);
    ppid == Some(1)
        && crate::sys::jail_pids(&run.mark).is_ok_and(|v| v.contains(&pid))
        && witnessed(run, pid)
}

/// Returns any processes in `mark` or `pids` still alive after 5 seconds.
#[must_use]
pub fn jail_left(mark: &str, pids: &[i32]) -> String {
    let mut left = String::new();
    for _ in 0..20 {
        let mut words: Vec<String> = match crate::sys::jail_pids(mark) {
            Ok(v) => v.iter().map(ToString::to_string).collect(),
            Err(e) => vec![e],
        };
        words.extend(
            pids.iter()
                .filter(|&&p| crate::procs::alive(p))
                .map(ToString::to_string),
        );
        left = words.join(" ");
        if left.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    left
}

/// Formats the jail's members as `pid ppid command` rows, or the one line saying why they were
/// not read.
#[must_use]
pub fn members_table(members: &Result<Vec<i32>, String>) -> String {
    let pids = match members {
        Ok(pids) if pids.is_empty() => return String::new(),
        Ok(pids) => pids,
        Err(why) => return format!("  not read: {why}\n"),
    };
    crate::procs::rows_of(pids)
        .into_iter()
        .map(|(pid, ppid, comm)| {
            let line = format!("{pid:>5} {ppid:>5} {comm}");
            format!("  {}\n", line.chars().take(120).collect::<String>())
        })
        .collect()
}

/// Open-file observation for a process ([`observe_open_files`]).
#[derive(Debug, Default)]
pub struct OpenFiles {
    /// The paths read, in `lsof -Fn`'s shape: `p<pid>`, then an `n<path>` line for each, a
    /// newline in a name written `\n`.
    pub text: String,
    /// Whether the whole table read.
    pub complete: bool,
}

/// The [`OpenFiles`] report of one read of `pid`'s paths: known paths retain the read's
/// completeness, and a failed read is empty and incomplete.
#[must_use]
pub fn open_files_report(pid: i32, read: std::io::Result<crate::procs::OpenPaths>) -> OpenFiles {
    let Ok(paths) = read else {
        return OpenFiles::default();
    };
    let mut text = format!("p{pid}\n");
    for path in paths.paths {
        text.push('n');
        text.push_str(&path.to_string_lossy().replace('\n', "\\n"));
        text.push('\n');
    }
    OpenFiles {
        text,
        complete: paths.complete,
    }
}

/// Observes the open files of agent process `pid` in this process, none of its sockets or
/// kqueues read ([`crate::procs::open_paths`], [`open_files_report`]).
#[must_use]
pub fn observe_open_files(pid: i32) -> OpenFiles {
    open_files_report(pid, crate::procs::open_paths(pid))
}

/// Owner home directories watched by lab witnesses ([`crate::paths::Paths::guarded_homes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watched(Vec<String>);

impl Watched {
    /// Collects the home directories guarded by `p`.
    #[must_use]
    pub fn of(p: &crate::paths::Paths) -> Watched {
        Watched(p.guarded_homes())
    }

    /// Returns the watched home paths.
    #[must_use]
    pub fn homes(&self) -> &[String] {
        &self.0
    }

    /// Returns `<home>/<rel>` for each watched home ([`owner_dirs`]).
    #[must_use]
    pub fn dirs(&self, rel: &str) -> Vec<String> {
        owner_dirs(&self.0, rel)
    }
}

/// Returns `<home>/<rel>` for each home in `homes` ([`crate::paths::Paths::guarded_homes`]).
#[must_use]
pub fn owner_dirs(homes: &[String], rel: &str) -> Vec<String> {
    homes.iter().map(|h| format!("{h}/{rel}")).collect()
}

/// Returns the allowed path prefixes inside `homes` for an agent in `run`.
#[must_use]
pub fn lab_roots(run: &Run, bin: &str, homes: &[String]) -> Vec<String> {
    let build = std::path::Path::new(bin)
        .parent()
        .and_then(std::path::Path::parent)
        .map(|b| b.to_string_lossy().into_owned())
        .filter(|b| {
            homes.iter().any(|h| {
                b.starts_with(&format!("{h}/Library/Application Support/aterm/pkg/store/"))
            })
        });
    std::iter::once(run.run.to_string_lossy().into_owned())
        .chain(build.or_else(|| (!bin.is_empty()).then(|| bin.to_string())))
        .collect()
}

/// Returns files in `lsof_fn` under `homes` that fall outside `allowed`.
#[must_use]
pub fn owner_strays(lsof_fn: &str, homes: &[String], allowed: &[String]) -> Vec<String> {
    let below = |path: &str, root: &str| {
        !root.is_empty()
            && path
                .strip_prefix(root)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    let mut out: Vec<String> = lsof_fn
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter(|p| homes.iter().any(|h| below(p, h)) && !allowed.iter().any(|a| below(p, a)))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Evaluates the stray-file check across `strays`, `pids`, and `observed` ([`owner_strays`]).
#[must_use]
pub const fn strays_verdict(strays: &[String], pids: &[i32], observed: bool) -> Verdict {
    if !strays.is_empty() {
        Verdict::Fail
    } else if pids.is_empty() || !observed {
        Verdict::Unproven
    } else {
        Verdict::Ok
    }
}

/// Run the cycle and the witnesses.
fn checks(run: &Run) -> Result<Checks, Fail> {
    let open_control = crate::recorder::control(run);
    println!("== survey cycle (aterm-lab scn's selftest cycle)");
    let cycle = crate::scn::run_selftest(&run.tag)?;
    let door = Door::new(run)?;
    let stub_log = crate::util::scratch_text(&run.w.join("stub.log"));
    println!("== stub log");
    for l in stub_log.lines().filter(|l| !l.contains("PROXY refused")) {
        println!("{}", l.chars().take(200).collect::<String>());
    }
    println!("== screen");
    let screen = door.out(&["text", "trim"]);
    for l in screen.lines().take(16) {
        println!("{l}");
    }
    println!("== egress");
    for l in crate::verbs::egress_text(run, "3").lines().filter(|l| {
        l.contains("EXTERNAL endpoints") || l.contains("kernel sandbox") || l.contains("deny")
    }) {
        println!("{}", l.chars().take(200).collect::<String>());
    }
    println!("== lsof -nP -i of the lab's processes (the claude row is the real Claude Code)");
    let lsof = crate::verbs::lsof_text(run).unwrap_or_default();
    let _ = crate::util::write_fresh(&run.run.join("lsof.txt"), &lsof);
    for l in lsof.lines() {
        println!("  {l}");
    }
    println!("== keychain tripwire (the account is masked)");
    print!("{}", crate::verbs::security_masked(run));
    let opened = crate::recorder::Witness::read(run, &open_control);
    opened.print();
    println!(
        "== isolation probes (this lab's rules on a replica home: nothing of the owner's is touched)"
    );
    let (iso, probe_text) = crate::probe::run_captured(&run.tag, true)?;
    let _ = crate::util::write_fresh(&run.run.join("probe.txt"), &probe_text);
    for l in probe_text.lines() {
        println!("  {l}");
    }
    let twin_ok = crate::verbs::twin_link_refused(run);
    let orphan = jail_orphan(run);
    let orphan_ok = orphan > 1 && orphan_seen(run, orphan);
    let members = crate::sys::jail_pids(&run.mark);
    println!(
        "== the jail's members, by its mark (pid, ppid, command; the orphan control is {orphan}, adopted by launchd)"
    );
    print!("{}", members_table(&members));
    let before = members.unwrap_or_default();
    println!("== the witness's lines for the orphan");
    for l in crate::util::read_regular_text(&run.run.join("sockets.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(&format!("[{orphan}]")))
    {
        println!("  {l}");
    }
    println!("== checks");
    let (mut n, mut bad) = (0, 0);
    let mut check = |label: &str, ok: bool| {
        n += 1;
        if ok {
            println!("  ok   {label}");
        } else {
            bad += 1;
            println!("  FAIL {label}");
        }
    };
    check(
        "Claude Code started in the tab and drew its composer",
        cycle != 2,
    );
    let answered = stub_log
        .lines()
        .any(|l| l.contains("POST /v1/messages -> 200 ") && l.contains("kind=main"))
        && door.out(&["text"]).contains("Hi! I am the local lab stub");
    check(
        "the turn \"hi\" was answered by the stub (a kind=main 200 in its log) and the TUI shows its reply",
        answered,
    );
    check(
        "the forced card showed, the card went, the harness dismissed it (DISMISSED journaled), the composer is empty",
        cycle == 0,
    );
    let sockets = crate::util::read_regular_text(&run.run.join("sockets.log")).unwrap_or_default();
    let egress = crate::util::read_regular_text(&run.run.join("egress.log")).unwrap_or_default();
    let claude_talked = sockets.lines().any(|l| {
        l.split_once(" claude[")
            .and_then(|(_, rest)| rest.split_once("] TCP conn 127.0.0.1:"))
            .is_some_and(|(pid, _)| !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
    });
    check(
        "no non-loopback socket was seen, and the witness did see Claude Code talk to the stub (egress)",
        claude_talked && !egress.contains("EXTERNAL"),
    );
    let sec = crate::util::scratch_text(&run.w.join("security.log"));
    let unsuffixed = sec.contains("service=\"Claude Code\"")
        || sec.contains("service=\"Claude Code-credentials\"");
    check(
        "the keychain tripwire was reached by Claude Code and never saw the owner's un-suffixed items",
        !sec.is_empty() && !unsuffixed,
    );
    check(crate::recorder::NOTHING_OPENED, opened.ok());
    check("the isolation probes passed", iso == 0);
    check(
        "twin refused the scratch's home/Library made a link to a decoy from inside the jail, and wrote nothing through it",
        twin_ok,
    );
    check(
        &format!(
            "an orphan in the jail (pid {orphan}: its shell exited, launchd adopted it) was found by the jail's mark and \
             seen by the witness connecting to the stub"
        ),
        orphan_ok,
    );
    Ok(Checks { n, bad, before })
}

#[cfg(test)]
#[path = "selftest_tests.rs"]
mod tests;
