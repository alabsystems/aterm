// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `selftest [tag] [--keep]`: end-to-end lab self-test driving Claude Code, [`crate::settings_witness`], and jail/stray checks.
use std::fs;
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
    fs::read_to_string(run.run.join("sockets.log"))
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

/// Formats `pids` as `pid ppid command` rows.
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

/// Validates `lsof -Fn` output shape and returns its text if non-empty.
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

/// Open-file observation for a process ([`observe_open_files`]).
#[derive(Debug, Default)]
pub struct OpenFiles {
    /// Captured `lsof -Fn` output.
    pub text: String,
    /// Whether strict stdout capture and shape validation both succeeded.
    pub complete: bool,
}

/// Builds an uncertified diagnostic [`OpenFiles`] report.
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

/// Builds a certified [`OpenFiles`] report from strict stdout capture.
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

/// Observes open files of agent process `pid` via `lsof` ([`complete_open_files`]).
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
    let _ = fs::write(run.run.join("lsof.txt"), &lsof);
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
mod tests {
    use super::*;

    /// A file of the owner's home that the lab never handed the agent is a
    /// stray: Muse Code 1.4.10's `session-names.db`, opened with HOME pointed
    /// at the scratch.
    #[test]
    fn a_file_of_the_owners_home_outside_the_lab_is_a_stray() {
        let home = &["/Users//o".to_string()];
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
        let at_top = "/Users//o/Library/Application Support/aterm/pkg/store/bin/x";
        assert_eq!(
            lab_roots(&run, at_top, home),
            ["/Users//o/Developer/lab/runs/t", at_top]
        );
    }

    /// UNDER A SCRATCH `HOME` THE ACCOUNT'S HOME IS WATCHED TOO: a file of the
    /// account's own home (the account database's) an agent holds is a stray
    /// whatever `HOME` said, as is one of the scratch home's, and an agent in
    /// the account's package store keeps its own build.
    #[test]
    fn under_a_scratch_home_a_file_of_the_account_home_is_a_stray() {
        let account = "/Users//o".to_string();
        let scratch = "/private/tmp/s/home".to_string();
        let homes = crate::paths::guarded(&account, &scratch);
        assert_eq!(homes, [account.clone(), scratch.clone()]);
        let run = Run {
            run: "/Users//o/Developer/lab/runs/t".into(),
            ..Run::default()
        };
        let bin = "/Users//o/Library/Application Support/aterm/pkg/store/claude/1/bin/claude";
        let roots = lab_roots(&run, bin, &homes);
        assert_eq!(
            roots,
            [
                "/Users//o/Developer/lab/runs/t",
                "/Users//o/Library/Application Support/aterm/pkg/store/claude/1",
            ]
        );
        let lsof = format!(
            "p7\nftxt\nn{bin}\nf3\nn/Users//o/.gemini/settings.json\nf4\nn/private/tmp/s/home/.zsh_history\n\
             f5\nn/Users//o/Developer/lab/runs/t/scratch/home/x\n"
        );
        assert_eq!(
            owner_strays(&lsof, &homes, &roots),
            [
                "/Users//o/.gemini/settings.json",
                "/private/tmp/s/home/.zsh_history"
            ]
        );
        let env_only = crate::paths::guarded("", &scratch);
        assert_eq!(
            owner_strays(&lsof, &env_only, &lab_roots(&run, bin, &env_only)),
            ["/private/tmp/s/home/.zsh_history"]
        );
        assert_eq!(
            lab_roots(&run, bin, &env_only),
            ["/Users//o/Developer/lab/runs/t", bin]
        );
    }

    /// EVERY WITNESS WATCHES EVERY HOME THE LAB GUARDS: [`Watched::of`] the
    /// lab's facts under a scratch `HOME` (a fake account home handed in
    /// through [`crate::paths::Paths::resolve_with`]) is the account's home
    /// and the scratch one, so the Codex, Gemini and agy witnesses look for
    /// `.codex` and `.gemini` in both, the Muse Code witness looks for its
    /// directories in both, and a file of either an agent holds is a stray.
    #[test]
    fn the_witnesses_watch_every_home_the_lab_guards() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-watched-")
            .tempdir()
            .unwrap();
        let real = std::fs::canonicalize(top.path()).unwrap();
        let (account, scratch) = (real.join("account"), real.join("scratch"));
        for d in [&account, &scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (a, s) = (account.display().to_string(), scratch.display().to_string());
        let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
        let w = Watched::of(&p);
        assert_eq!(w.homes(), [a.clone(), s.clone()]);
        for rel in [".codex", ".gemini"] {
            assert_eq!(w.dirs(rel), [format!("{a}/{rel}"), format!("{s}/{rel}")]);
        }
        let run = Run {
            run: format!("{a}/lab/runs/t").into(),
            ..Run::default()
        };
        let roots = lab_roots(&run, "", w.homes());
        let lsof = format!(
            "p7\nf3\nn{a}/.gemini/settings.json\nf4\nn{s}/.codex/auth.json\nf5\nn{a}/lab/runs/t/scratch/x\n"
        );
        assert_eq!(
            owner_strays(&lsof, w.homes(), &roots),
            [
                format!("{a}/.gemini/settings.json"),
                format!("{s}/.codex/auth.json")
            ]
        );
        let muse = format!("p7\nf3\nn{a}/.local/share/muse/db\nf4\nn{s}/.config/muse/auth.json\n");
        assert_eq!(
            crate::muse_proof::held_under(&muse, w.homes()),
            [
                format!("{a}/.local/share/muse/db"),
                format!("{s}/.config/muse/auth.json")
            ]
        );
        let own = Watched::of(&crate::paths::Paths::resolve_with(
            Some(account.clone()),
            Some(account.into()),
        ));
        assert_eq!(own.homes(), std::slice::from_ref(&a));
        assert_eq!(own.dirs(".codex"), [format!("{a}/.codex")]);
        assert_eq!(
            owner_strays(&lsof, own.homes(), &roots),
            [format!("{a}/.gemini/settings.json")]
        );
        assert_eq!(
            crate::muse_proof::held_under(&muse, own.homes()),
            [format!("{a}/.local/share/muse/db")]
        );
    }

    /// THE STRAYS CHECK FAILS ON A STRAY whatever else it saw, holds on a
    /// complete report of every agent process with none, and is UNPROVEN with
    /// no agent process or an incomplete report.
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
                &owner_strays(&seen.text, &["/Users//owner".to_string()], &[]),
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
        let held = open_files(7, Ok(out(b"p7\nn/Users//owner/private.txt\n")));
        assert_eq!(
            strays_verdict(
                &owner_strays(&held.text, &["/Users//owner".to_string()], &[]),
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
