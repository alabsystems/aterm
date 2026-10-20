// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `selftest [tag] [--keep]`: the whole proof in one command (about 25 s). It
//! brings a lab up, launches Claude Code in the first tab (the harness answers
//! the folder-trust dialog), takes one `hi` turn the stub answers, forces the
//! session-survey card, lets the harness dismiss it, then checks eight things,
//! takes the lab down and checks a ninth and a tenth: the TUI drew its composer; the stub
//! answered and the TUI shows the reply; the card showed, went, `DISMISSED
//! survey` was journaled and the composer was left empty (the harness's `0` is
//! timed and printed, not required); no non-loopback socket was seen, and the
//! egress witness did see Claude Code talk to the stub (a witness that saw
//! nothing proves nothing); the keychain tripwire was reached by Claude Code
//! and never saw the owner's un-suffixed items; the isolation probes pass on a
//! replica home (nothing of the owner's is touched); `twin` refuses the
//! scratch's `home/Library` made a link to a decoy from inside the jail and
//! writes nothing through it; an orphan in the jail (a jailed shell's child
//! that launchd adopted) is found by the jail's mark and seen by the witness
//! connecting to the stub; after `down` nothing in the jail is alive, asked
//! of the kernel by its mark; and the harness tapes the instance recorded of
//! its hosted loops (`crate::tape`), kept by `down`, each replay from
//! themselves alone. However it ends (a failed check, ^C, TERM)
//! the lab comes down; a SIGKILL ends its processes through their lifeline,
//! and `down` then removes its run directory (`ps` lists it).

use std::fs;
use std::time::Duration;

use crate::LAB;
use crate::codex_proof::Verdict;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::run::Run;
use crate::{Fail, Res, stop};

/// `selftest [tag] [--keep]`.
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
    // the lab ends with this run, however it ends, unless it is kept
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
            // The harness's tapes (`crate::tape`): the instance recorded each
            // hosted loop it ran, the survey cycle ended each as it finished
            // (its session's Autopilot off: `crate::tape::end_loops`), `down`
            // kept them with the logs, and each must run again from itself
            // alone: REPRODUCED.
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
        // the lab is ours to end on any way out: the next selftest finds nothing standing
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

/// What the checks found: how many ran, how many failed, and the jail's
/// members before `down` (the last check asks after them).
pub struct Checks {
    /// Checks run.
    pub n: u32,
    /// Checks failed.
    pub bad: u32,
    /// The jail's members by its mark, the orphan control among them.
    pub before: Vec<i32>,
}

/// The selftests' orphan control: a jailed shell starts a child and exits at
/// once, so launchd adopts the child and no ppid leads from the lab to it any
/// more (what a Bash tool's `nohup … &` or a run's leftover becomes). The
/// child holds a connection to the stub for two minutes. Its pid.
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
    fs::read_to_string(run.run.join("sockets.log"))
        .unwrap_or_default()
        .lines()
        .any(|l| l.ends_with(&want))
}

/// Whether launchd adopted `pid`, the jail's mark finds it, and the witness logged its connection.
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

/// What of the jail of `mark`, and of `pids`, is still alive (nothing within 5 s: empty).
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

/// The jail's members by its mark, as `pid ppid command` rows for the report.
#[must_use]
pub fn members_table(pids: &[i32]) -> String {
    if pids.is_empty() {
        return String::new();
    }
    let list: Vec<String> = pids.iter().map(ToString::to_string).collect();
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-o", "pid=,ppid=,comm=", "-p", &list.join(",")])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| format!("  {}\n", l.chars().take(120).collect::<String>()))
                .collect()
        })
        .unwrap_or_default()
}

/// Validate the existing lsof report shape after capture. This text check
/// does not establish EOF: only `complete_open_files` receives that proof. A
/// failed, malformed or capture-ceiling report remains unknown.
#[must_use]
pub fn lsof_report(
    pid: i32,
    output: Result<std::process::Output, aterm_uds::deadline::RunError>,
) -> Option<String> {
    let out = output.ok()?;
    if !out.status.success()
        || !out.stderr.is_empty()
        || out.stdout.len() >= aterm_uds::deadline::OUTPUT_KEEP
    {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    (text.starts_with(&format!("p{pid}\n")) && text.ends_with('\n')).then_some(text)
}

/// One open-file observation of a process ([`observe_open_files`]).
#[derive(Debug, Default)]
pub struct OpenFiles {
    /// Whatever `lsof -Fn` printed: a failed or partial report still shows
    /// the files it names, and a file held is evidence however it was shown.
    pub text: String,
    /// Whether strict stdout capture and the report-shape checks both
    /// succeeded: only that can prove an absence.
    pub complete: bool,
}

/// A diagnostic (legacy tail-capture) report. Its positive paths remain
/// evidence, but without an EOF certificate it cannot prove an absence.
#[cfg(test)]
#[must_use]
pub fn open_files(
    pid: i32,
    output: Result<std::process::Output, aterm_uds::deadline::RunError>,
) -> OpenFiles {
    describe_open_files(pid, output, false)
}

fn describe_open_files(
    pid: i32,
    output: Result<std::process::Output, aterm_uds::deadline::RunError>,
    captured_complete: bool,
) -> OpenFiles {
    let text = output
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    OpenFiles {
        complete: captured_complete && lsof_report(pid, output).is_some(),
        text,
    }
}

/// A result from the strict stdout capture lane: only its success proves
/// stdout reached EOF without discarded bytes or a read error.
#[must_use]
pub fn complete_open_files(
    pid: i32,
    output: Result<std::process::Output, aterm_uds::deadline::CompleteOutputError>,
) -> OpenFiles {
    match output {
        Ok(out) => describe_open_files(pid, Ok(out), true),
        Err(_) => OpenFiles::default(),
    }
}

/// The open files of one of this lab's observed agent processes
/// ([`complete_open_files`]). The existing strict capture bounds execution
/// and stdout storage and rejects read errors, missing EOF and overflow.
/// Stderr remains diagnostic; the shared child/grace/cleanup contract applies.
/// No installed agent, provider or owner file is executed or opened.
#[must_use]
pub fn observe_open_files(pid: i32) -> OpenFiles {
    let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/lsof");
    cmd.args(["-p", &pid.to_string(), "-Fn"]);
    complete_open_files(
        pid,
        aterm_uds::deadline::output_until_complete_stdout(
            &mut cmd,
            std::time::Instant::now() + Duration::from_secs(60),
        ),
    )
}

/// The roots of the owner's home `real_home` an agent of `run` may hold files
/// under: the run directory (its scratch HOME, its launcher) and, for a binary
/// `bin` in aterm's package store (which keeps the managed agents in the
/// owner's home, and of which the jail re-allows only that build), the build
/// it sits in (the directory above `bin/`). A binary anywhere else adds only
/// itself: the directory two above `~/bin/muse` is the home.
#[must_use]
pub fn lab_roots(run: &Run, bin: &str, real_home: &str) -> Vec<String> {
    let store = format!("{real_home}/Library/Application Support/aterm/pkg/store/");
    let build = std::path::Path::new(bin)
        .parent()
        .and_then(std::path::Path::parent)
        .map(|b| b.to_string_lossy().into_owned())
        .filter(|b| b.starts_with(&store));
    std::iter::once(run.run.to_string_lossy().into_owned())
        .chain(build.or_else(|| (!bin.is_empty()).then(|| bin.to_string())))
        .collect()
}

/// The files an `lsof -Fn` report names in the owner's home `real_home` that
/// lie under none of `allowed` ([`lab_roots`]): what an agent holds of the
/// owner's that the lab never handed it, each once. The jail denies writes
/// there but reads only of what is on its protected list, so a file named here
/// is a gap in that list.
#[must_use]
pub fn owner_strays(lsof_fn: &str, real_home: &str, allowed: &[String]) -> Vec<String> {
    let below = |path: &str, root: &str| {
        !root.is_empty()
            && path
                .strip_prefix(root)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    let mut out: Vec<String> = lsof_fn
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter(|p| below(p, real_home) && !allowed.iter().any(|a| below(p, a)))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The strays check: a file of the owner's home outside the lab's roots that
/// an agent process holds ([`owner_strays`]) fails it; with none, it holds
/// when each agent process seen (`pids`) gave a complete open-file report
/// (`observed`), and is UNPROVEN otherwise.
#[must_use]
pub fn strays_verdict(strays: &[String], pids: &[i32], observed: bool) -> Verdict {
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
    // The cycle is the scenario runner's: it attaches to this lab, launches Claude Code, takes the "hi" turn,
    // forces the survey card live and judges it from the frame stream. 0 held, 1 not, 2 no composer.
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
    let _ = fs::write(run.run.join("lsof.txt"), &lsof);
    for l in lsof.lines() {
        println!("  {l}");
    }
    println!("== keychain tripwire (the account is masked)");
    print!("{}", crate::verbs::security_masked(run));
    println!(
        "== isolation probes (this lab's rules on a replica home: nothing of the owner's is touched)"
    );
    let (iso, probe_text) = crate::probe::run_captured(&run.tag, true)?;
    let _ = fs::write(run.run.join("probe.txt"), &probe_text);
    for l in probe_text.lines() {
        println!("  {l}");
    }
    let twin_ok = crate::verbs::twin_link_refused(run);
    let orphan = jail_orphan(run);
    let orphan_ok = orphan > 1 && orphan_seen(run, orphan);
    let before = crate::sys::jail_pids(&run.mark).unwrap_or_default();
    println!(
        "== the jail's members, by its mark (pid, ppid, command; the orphan control is {orphan}, adopted by launchd)"
    );
    print!("{}", members_table(&before));
    println!("== the witness's lines for the orphan");
    for l in fs::read_to_string(run.run.join("sockets.log"))
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
    let sockets = fs::read_to_string(run.run.join("sockets.log")).unwrap_or_default();
    let egress = fs::read_to_string(run.run.join("egress.log")).unwrap_or_default();
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
mod tests {
    use super::*;

    /// A file of the owner's home that the lab never handed the agent is a
    /// stray: Muse Code 1.4.10's `session-names.db`, opened with HOME pointed
    /// at the scratch. NEGATIVE CONTROLS: the run directory (the scratch HOME),
    /// the agent's own build in the package store, a sibling whose name only
    /// begins like the home, and paths outside the home are not. A binary
    /// elsewhere in the home (`LAB_MUSE=~/bin/muse`) allows only itself, never
    /// the directory two above it (the home).
    #[test]
    fn a_file_of_the_owners_home_outside_the_lab_is_a_stray() {
        let home = "/Users//o";
        let run = Run {
            run: "/Users//o/Developer/lab/runs/t".into(),
            ..Run::default()
        };
        let bin = "/Users//o/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";
        let allowed = lab_roots(&run, bin, home);
        assert_eq!(
            allowed,
            [
                "/Users//o/Developer/lab/runs/t",
                "/Users//o/Library/Application Support/aterm/pkg/store/muse/2001004010006427002",
            ]
        );
        let db =
            "/Users//o/Library/Application Support/Muse/session-name-authority/session-names.db";
        let lsof = format!(
            "p7\nfcwd\nn/private/tmp/at.x/proj\nftxt\nn{bin}\nf3\nn/Users//o/Developer/lab/runs/t/scratch/home/.config/muse/x\n\
             f4\nn{db}\nf5\nn/Users//ox/notes\nf6\nn/private/tmp/tbh-501-rt/muse/ms-1.sock.lease\nf7\nn{db}\n\
             f8\nn/Users//o/Developer/lab/runs/tt/x\n"
        );
        assert_eq!(
            owner_strays(&lsof, home, &allowed),
            ["/Users//o/Developer/lab/runs/tt/x", db]
        );
        let clean = format!("p7\nftxt\nn{bin}\nf3\nn/Users//o/Developer/lab/runs/t/scratch/x\n");
        assert!(
            owner_strays(&clean, home, &allowed).is_empty(),
            "the control"
        );
        // an agent outside the home (Homebrew's Gemini CLI) adds no root in it, and the home itself held is a stray
        let gemini = lab_roots(
            &run,
            "/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/bin/gemini",
            home,
        );
        assert_eq!(
            gemini,
            [
                "/Users//o/Developer/lab/runs/t",
                "/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/bin/gemini",
            ]
        );
        assert_eq!(
            owner_strays("p7\nfcwd\nn/Users//o\n", home, &gemini),
            ["/Users//o"]
        );
        // a binary two below the home, or elsewhere in it outside the store, allows itself and nothing beside it
        for own in [
            "/Users//o/bin/muse",
            "/Users//o/.local/bin/muse",
            "/Users//o/Library/Application Support/aterm/pkg/bin/muse",
        ] {
            let roots = lab_roots(&run, own, home);
            assert_eq!(roots, ["/Users//o/Developer/lab/runs/t", own], "{own}");
            let lsof = format!("p7\nftxt\nn{own}\nf4\nn{db}\nf5\nn/Users//o/bin/other\n");
            assert_eq!(
                owner_strays(&lsof, home, &roots),
                [db, "/Users//o/bin/other"],
                "{own}"
            );
        }
        // the store itself is no build: a binary at store/bin/x allows only itself
        let at_top = "/Users//o/Library/Application Support/aterm/pkg/store/bin/x";
        assert_eq!(
            lab_roots(&run, at_top, home),
            ["/Users//o/Developer/lab/runs/t", at_top]
        );
    }

    /// THE STRAYS CHECK FAILS ON A STRAY whatever else it saw, holds on a
    /// complete report of every agent process with none, and is UNPROVEN with
    /// no agent process or an incomplete report. NEGATIVE CONTROL: those two
    /// shapes are never ok.
    #[test]
    fn the_strays_check_fails_on_a_stray_and_needs_a_complete_report() {
        let stray = vec!["/Users//o/Library/Application Support/Muse/x".to_string()];
        assert_eq!(strays_verdict(&stray, &[7], true), Verdict::Fail);
        assert_eq!(strays_verdict(&stray, &[], false), Verdict::Fail);
        assert_eq!(strays_verdict(&[], &[7], true), Verdict::Ok);
        assert_eq!(strays_verdict(&[], &[], true), Verdict::Unproven);
        assert_eq!(strays_verdict(&[], &[7], false), Verdict::Unproven);
    }
    /// Typed strict capture admits ordinary complete reports; a legacy
    /// diagnostic prefix has no EOF proof, even if it ends on a full line.
    #[test]
    fn an_open_file_absence_requires_proven_stdout_completion() {
        use aterm_uds::deadline::{CompleteOutputError, RunError};
        use std::os::unix::process::ExitStatusExt as _;
        let out = |bytes: &[u8]| std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: bytes.to_vec(),
            stderr: Vec::new(),
        };
        let clean = b"p7\nn/private/tmp/scratch/home/file\n";
        let seen = complete_open_files(7, Ok(out(clean)));
        assert!(seen.complete);
        assert_eq!(
            strays_verdict(
                &owner_strays(&seen.text, "/Users//owner", &[]),
                &[7],
                seen.complete
            ),
            Verdict::Ok
        );
        for failure in [
            CompleteOutputError::StdoutIncomplete,
            CompleteOutputError::StdoutRead(std::io::Error::other("injected read failure")),
            CompleteOutputError::StdoutTooLarge {
                limit: aterm_uds::deadline::OUTPUT_KEEP,
            },
            CompleteOutputError::Run(RunError::TimedOut),
        ] {
            let seen = complete_open_files(7, Err(failure));
            assert!(!seen.complete && seen.text.is_empty());
            assert_eq!(strays_verdict(&[], &[7], seen.complete), Verdict::Unproven);
        }
        // A diagnostic prefix naming a held owner file is still positive evidence.
        let held = open_files(7, Ok(out(b"p7\nn/Users//owner/private.txt\n")));
        assert_eq!(
            strays_verdict(
                &owner_strays(&held.text, "/Users//owner", &[]),
                &[7],
                held.complete
            ),
            Verdict::Fail
        );
        let mut admitted_prefixes = Vec::new();
        for bytes in [clean.as_slice(), b"p7\n".as_slice()] {
            let prefix = open_files(7, Ok(out(bytes)));
            if strays_verdict(&[], &[7], prefix.complete) != Verdict::Unproven {
                admitted_prefixes.push(String::from_utf8_lossy(bytes).into_owned());
            }
        }
        assert!(
            admitted_prefixes.is_empty(),
            "LAB-STDOUT-COMPLETENESS: {admitted_prefixes:?}"
        );
    }
}
