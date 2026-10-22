// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The proofs for Codex in the lab.
//!
//! `codex-selftest [tag] [--keep]` (about 20 s): a lab of its own (the harness
//! off: this is the jail's and Codex's proof, not the harness's), a
//! `codex-run` turn the stub holds 3 s (outside the instance's tree, where the
//! witness must watch it too), the real Codex launched in the tab on the lab
//! provider and one turn the stub answers, then the egress witness (both
//! Codex processes seen talking to the stub, nothing external), the owner's
//! `~/.codex` (never looked at: only the kernel's refusals, the decoy and what
//! Codex itself holds open are read), the open recorder, the isolation probes
//! on a replica home with this lab's rules, `twin` refusing a planted link, an
//! orphan in the jail found and witnessed, and after `down` nothing left in
//! the jail. Each witness has a control that shows it can see; one whose
//! control saw nothing is UNPROVEN, never ok. Exit 1 when a check fails.
//!
//! (`codex-captures`, which captured Codex's screens for aterm's Codex
//! screen reader, went with that reader on 2026-10-16: aterm reads no Codex
//! screen.)

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::LAB;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::paths::paths;
use crate::procs::JailOut;
use crate::run::Run;
use crate::selftest::{jail_left, jail_orphan, members_table, orphan_seen};
use crate::sys::{PutSrc, put};
use crate::util::scratch_text;
use crate::{Fail, Res, stop};

/// One check's result: held, failed, or unproven (its witness's control saw nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It held.
    Ok,
    /// It did not.
    Fail,
    /// Its witness's control saw nothing: no pass, and no failure.
    Unproven,
}

/// The tally of a selftest's checks.
#[derive(Debug, Default)]
pub struct Tally {
    n: u32,
    bad: u32,
    unproven: u32,
}

impl Tally {
    /// Print one check and count it.
    pub fn check(&mut self, label: &str, v: Verdict) {
        self.n += 1;
        match v {
            Verdict::Ok => println!("  ok   {label}"),
            Verdict::Unproven => {
                self.unproven += 1;
                println!("  ??   UNPROVEN {label}");
            }
            Verdict::Fail => {
                self.bad += 1;
                println!("  FAIL {label}");
            }
        }
    }

    /// Print `verb`'s verdict over every check counted: PASS (how many of
    /// them unproven) and 0, or FAIL and 1.
    ///
    /// # Errors
    /// None: a selftest's verdict is its exit status.
    pub fn verdict(&self, verb: &str) -> crate::Res {
        if self.bad == 0 {
            println!(
                "{verb}: PASS ({} checks{})",
                self.n,
                if self.unproven > 0 {
                    format!(", {} of them unproven", self.unproven)
                } else {
                    String::new()
                }
            );
            Ok(0)
        } else {
            println!("{verb}: FAIL ({} of {} checks)", self.bad, self.n);
            Ok(1)
        }
    }
}

fn ok(b: bool) -> Verdict {
    if b { Verdict::Ok } else { Verdict::Fail }
}

/// What the scratch HOME's decoy `.codex` holds: each entry's name, kind,
/// size and modification time, and the decoy file's bytes.
fn decoy_state(run: &Run) -> String {
    let d = run.w.join("home/.codex");
    let mut rows: Vec<String> = fs::read_dir(&d)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let m = e.path().symlink_metadata().ok();
                    format!(
                        "{} {:?} {} {:?}",
                        e.file_name().to_string_lossy(),
                        m.as_ref().map(fs::Metadata::file_type),
                        m.as_ref().map_or(0, fs::Metadata::len),
                        m.and_then(|m| m.modified().ok())
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort();
    rows.push(scratch_text(&d.join("LAB-DECOY")));
    rows.join("\n")
}

/// Every entry under `dir`, recursively (a link is not followed).
fn count_entries(dir: &Path) -> usize {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| {
            1 + if e.file_type().is_ok_and(|t| t.is_dir()) {
                count_entries(&e.path())
            } else {
                0
            }
        })
        .sum()
}

/// The Codex processes of this lab: those of its processes that run the Codex under test.
fn codex_pids(run: &Run) -> Vec<i32> {
    crate::procs::lab_tree(run)
        .into_iter()
        .filter(|&p| crate::procs::command_line(p).contains(&run.codex_bin))
        .collect()
}

/// Whether the witness saw one of `pids` (`codex[pid]`) connect to the stub.
fn codex_talked(run: &Run, pids: &[i32]) -> bool {
    let tail = format!("] TCP conn 127.0.0.1:{}", run.port);
    fs::read_to_string(run.run.join("sockets.log"))
        .unwrap_or_default()
        .lines()
        .any(|l| {
            l.ends_with(&tail)
                && l.split_once(" codex[").is_some_and(|(_, rest)| {
                    rest.split_once(']')
                        .and_then(|(p, _)| p.parse::<i32>().ok())
                        .is_some_and(|p| pids.contains(&p))
                })
        })
}

/// A custom mode the stub answers once, holding the turn `delay_ms` (Codex does
/// not keep its connection alive after an answer, and a 1 Hz witness must see it open).
fn held_turn_mode(run: &Run, name: &str, text: &str) -> Result<(), Fail> {
    let body =
        format!("{{\"times\": 1, \"delay_ms\": 3000, \"stream\": true, \"text\": \"{text}\"}}\n");
    put(
        &run.w.join(format!("modes/{name}.json")),
        0o644,
        &PutSrc::Bytes(body.as_bytes()),
    )
    .map_err(|e| Fail::die(format!("cannot write the {name} mode: {e}")))?;
    crate::verbs::set_mode(run, name)?;
    Ok(())
}

/// Whether the stub's log holds a line with every one of `parts`, in order.
fn stub_logged(run: &Run, parts: &[&str]) -> bool {
    scratch_text(&run.w.join("stub.log")).lines().any(|l| {
        let mut rest = l;
        parts.iter().all(|p| match rest.find(p) {
            Some(i) => {
                rest = &rest[i + p.len()..];
                true
            }
            None => false,
        })
    })
}

/// `codex-selftest [tag] [--keep]`.
pub fn cmd_codex_selftest(args: &[String]) -> Res {
    let (mut tag, mut keep) = ("codex-selftest".to_string(), false);
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
        harness_keys: "enabled = false".into(),
        lifeline: !keep,
        ..UpOpts::from_env()
    };
    let run = crate::lab::up(&tag, &o).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            Fail::die(format!(
                "codex-selftest: up failed: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    // the lab it brought up does not outlive it, however it ends
    let result = selftest_checks(&run);
    let mut t = match result {
        Ok(t) => t,
        Err(f) => {
            let _ = crate::lab::down(&tag, false);
            return Err(f);
        }
    };
    let (before, pids, orphan) = (t.before.clone(), t.pids.clone(), t.orphan);
    if keep {
        println!("codex-selftest: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let k = crate::lab::down(&tag, false)?;
        println!("lab '{tag}' is down; logs kept in {}", k.display());
        // asked afresh after down, of the kernel, by the jail's mark: not the pids collected before it (a
        // leftover of codex-run, which had exited, was in none of them)
        let mut asked: Vec<i32> = before.clone();
        asked.extend(&pids);
        let left = jail_left(&run.mark, &asked);
        let pid_list: Vec<String> = pids.iter().map(ToString::to_string).collect();
        t.tally.check(
            &format!(
                "nothing in the lab's jail is alive after down, asked of the kernel by its mark (before it: the tab's \
                 Codex {}, the orphan {orphan}, {} processes in all){}",
                if pid_list.is_empty() { "none".into() } else { pid_list.join(" ") },
                before.len(),
                if left.is_empty() { String::new() } else { format!(": alive {left}") }
            ),
            ok(before.contains(&orphan) && !pids.is_empty() && left.is_empty()),
        );
    }
    t.tally.verdict("codex-selftest")
}

/// What the checks before `down` leave for the last one.
struct Before {
    tally: Tally,
    before: Vec<i32>,
    pids: Vec<i32>,
    orphan: i32,
}

#[allow(clippy::too_many_lines)]
fn selftest_checks(run: &Run) -> Result<Before, Fail> {
    if run.codex_bin.is_empty() {
        return Err(Fail::die(
            "codex-selftest: no managed Codex here (aterm pkg install codex, or LAB_CODEX=/path/to/codex)",
        ));
    }
    // the owner's homes: the account's, whatever HOME says, and HOME's when it names another
    let watched = crate::selftest::Watched::of(paths());
    let decoy0 = decoy_state(run);
    // The decoy's control: a jailed Codex started with NO CODEX_HOME and HOME a second scratch home writes into
    // that home's .codex at once (it makes tmp/arg0 there at every start, --version too), so a lab Codex that fell
    // back to $HOME/.codex would change the decoy. The refusals' control: the same jailed Codex with its HOME in the
    // run directory, which the jail refuses it; the unified log keeps such a report or drops it (on macOS 27 it kept
    // mach-lookup and process-exec refusals and no file refusal at all), so its silence about ~/.codex counts only
    // when it kept the control. The recorder's control: a bare-name `open` in the jail, on the lab Codex's PATH,
    // lands on the recorder and is logged.
    let ctlname = format!(
        "lab-control-refusal-{}-{}",
        std::process::id(),
        crate::util::now_secs()
    );
    let ctlhome = run.w.join("control-home");
    let codex = run.codex_bin.as_str();
    let _ = crate::procs::jailed(
        run,
        &run.w,
        &[("HOME", &ctlhome.to_string_lossy())],
        &[codex, "--version"],
        JailOut::Capture,
    );
    let ctlmade = count_entries(&ctlhome.join(".codex"));
    let refused_home = run.run.join(&ctlname);
    let _ = crate::procs::jailed(
        run,
        &run.w,
        &[("HOME", &refused_home.to_string_lossy())],
        &[codex, "--version"],
        JailOut::Capture,
    );
    let open_control = crate::recorder::control(run);
    println!(
        "== codex {} in lab '{}' (the lab provider: base_url http://127.0.0.1:{}/v1, a fake env_key)",
        run.codex_ver, run.tag, run.port
    );
    // codex-run's Codex runs OUTSIDE the instance's tree, and the witness must watch it too: one `exec` turn the
    // stub holds 3 s, before the tab's Codex is up; its pid is the last of jailed.pids
    held_turn_mode(
        run,
        "selftest-run",
        "Hi! I am the local lab stub, not a model. (the codex-run turn, held 3 s)",
    )?;
    let mut ra: Vec<String> = ["--", "exec", "--skip-git-repo-check", "hi"]
        .map(String::from)
        .to_vec();
    let (runrc, runout) = crate::codex::codex_run(run, &mut ra, JailOut::CaptureAll)?;
    let runpid = crate::procs::jailed_roots(&run.run, true)
        .last()
        .copied()
        .unwrap_or(0);
    println!(
        "== codex-run (pid {runpid}, exit {runrc}): {}",
        runout
            .lines()
            .rev()
            .find(|l| l.contains("the codex-run turn"))
            .unwrap_or("")
    );
    let mut runok = false;
    if runrc == 0
        && runout.contains("the codex-run turn, held 3 s")
        && stub_logged(
            run,
            &[
                "POST /v1/responses -> 200 ",
                "mode=selftest-run kind=codex auth=key",
            ],
        )
    {
        for _ in 0..6 {
            if codex_talked(run, &[runpid]) {
                runok = true;
                break;
            }
            stop::sleep(Duration::from_millis(500))?;
        }
    }
    let l = crate::codex::Launch {
        mode: "lab".into(),
        plan: "plus".into(),
        no_wait: true,
        ..crate::codex::Launch::default()
    };
    crate::codex::launch(run, &l).map_err(|f| {
        Fail::die(format!(
            "codex-selftest: could not launch Codex: {}",
            f.msg.unwrap_or_default()
        ))
    })?;
    let run = &Run::load(&run.tag)?;
    let door = Door::new(run)?;
    // the composer itself (a row that begins with `›`), not the header above it, which draws while Codex starts
    let drew = door.seen("^›", 60)?;
    let mut answered = false;
    if drew {
        held_turn_mode(
            run,
            "selftest",
            "Hi! I am the local lab stub, not a model. (the selftest turn, held 3 s)",
        )?;
        answered = crate::verbs::turn(&door, "hi")?
            && door.seen("the selftest turn, held 3 s", 30)?
            && stub_logged(
                run,
                &[
                    "POST /v1/responses -> 200 ",
                    "mode=selftest kind=codex auth=key",
                ],
            );
    }
    // the TAB's rollout of its turn, under the scratch CODEX_HOME: Codex kept to it. codex-run's is there too and is
    // never taken (exec's are skipped, and only the tab's turn holds the selftest text)
    let mut rollout = false;
    for _ in 0..20 {
        if let Some(f) = crate::codex::tab_rollout(run) {
            let t = scratch_text(&f);
            if t.contains("the selftest turn, held 3 s") && t.contains("\"token_count\"") {
                rollout = true;
                break;
            }
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    // the witness must have seen the tab Codex's own connection to the stub, under its pid
    let pids = codex_pids(run);
    for _ in 0..20 {
        if codex_talked(run, &pids) {
            break;
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    println!("== stub log");
    for l in scratch_text(&run.w.join("stub.log"))
        .lines()
        .filter(|l| !l.contains("PROXY refused"))
    {
        println!("{}", l.chars().take(200).collect::<String>());
    }
    println!("== screen");
    for l in door
        .out(&["text", "trim"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(12)
    {
        println!("{l}");
    }
    println!("== egress");
    for l in crate::verbs::egress_text(run, "3").lines().filter(|l| {
        l.contains("EXTERNAL endpoints")
            || l.contains("kernel sandbox")
            || l.contains("deny")
            || l.contains("PROXY")
    }) {
        println!("{}", l.chars().take(200).collect::<String>());
    }
    println!(
        "== sockets of the lab's Codex processes, the tab's and codex-run's (the witness's lines for them)"
    );
    for l in fs::read_to_string(run.run.join("sockets.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(" codex["))
    {
        println!("  {l}");
    }
    let opened = crate::recorder::Witness::read(run, &open_control);
    opened.print();
    // ~/.codex: what the kernel refused naming it, what Codex holds open, and the decoy of the scratch HOME
    let mut klog = String::new();
    let mut control = false;
    for _ in 0..3 {
        // a report reaches the log within a second or so, when it is kept
        klog = crate::verbs::sandbox_log("3");
        if klog.contains(&ctlname) {
            control = true;
            break;
        }
        stop::sleep(Duration::from_secs(1))?;
    }
    // the refusals of THIS lab's processes (the controls and codex-runs among them): another lab, the probes' own
    // jailed reach for ~/.codex, any other sandboxed process of the owner are not this lab's Codex
    let pat: Vec<String> = crate::procs::lab_pids_ever(run)
        .iter()
        .map(ToString::to_string)
        .collect();
    let owner_codex = watched.dirs(".codex");
    let denied: Vec<String> = klog
        .lines()
        .filter_map(|l| crate::verbs::refusal_of(l, &pat))
        .filter(|l| owner_codex.iter().any(|c| l.contains(c.as_str())))
        .take(5)
        .collect();
    let held: Vec<String> = pids
        .iter()
        .flat_map(|p| {
            aterm_uds::spawnfd::command("/usr/sbin/lsof")
                .args(["-p", &p.to_string(), "-Fn"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
                .lines()
                .filter(|l| owner_codex.iter().any(|c| l.starts_with(&format!("n{c}"))))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .take(5)
        .collect();
    let decoy1 = decoy_state(run);
    println!(
        "== the owner's ~/.codex (never opened here: the kernel's refusals naming it, Codex's open files under it, the scratch HOME's decoy)"
    );
    println!(
        "  kernel refusals naming it: {} (the log {} this lab's control refusal, a jailed Codex's write into the run directory)",
        if denied.is_empty() {
            "none".into()
        } else {
            denied.join(" | ")
        },
        if control { "kept" } else { "dropped" }
    );
    let pid_list: Vec<String> = pids.iter().map(ToString::to_string).collect();
    println!(
        "  open under it, by the lab's Codex (pids {}): {}",
        if pid_list.is_empty() {
            "none".into()
        } else {
            pid_list.join(" ")
        },
        if held.is_empty() {
            "none".into()
        } else {
            held.join(" ")
        }
    );
    println!(
        "  the decoy {}; its control (a Codex with no CODEX_HOME) made {ctlmade} entries in its own home's .codex",
        if decoy0 == decoy1 {
            "is unchanged (only LAB-DECOY)"
        } else {
            "CHANGED"
        }
    );
    println!(
        "== isolation probes (the rules of this lab's jail, Codex's among them, rendered for a replica home: nothing of the owner's is touched)"
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
    let home_ok = ctlmade > 0 && held.is_empty() && decoy0 == decoy1 && !pids.is_empty();
    let refused = if !denied.is_empty() {
        Verdict::Fail
    } else if control {
        Verdict::Ok
    } else {
        Verdict::Unproven
    };
    let open_ok = opened.ok();
    let egress = fs::read_to_string(run.run.join("egress.log")).unwrap_or_default();
    let egress_ok = codex_talked(run, &pids) && !egress.contains("EXTERNAL");
    println!("== checks");
    let mut t = Tally::default();
    t.check(
        &format!(
            "Codex {} started in the tab and drew its composer",
            run.codex_ver
        ),
        ok(drew),
    );
    t.check(
        "the turn \"hi\" was answered by the stub (a kind=codex auth=key 200 in its log) and the TUI shows its reply",
        ok(answered),
    );
    t.check(
        "the tab's turn is in the tab's own rollout under the scratch CODEX_HOME (its reply and a token_count; codex-run's rollout is not taken)",
        ok(rollout),
    );
    t.check(
        &format!(
            "no non-loopback socket was seen, and the egress witness did see the tab's Codex connect to the stub (port {})",
            run.port
        ),
        ok(egress_ok),
    );
    t.check(
        &format!(
            "codex-run's Codex (pid {runpid}, outside the instance's tree) took its exec turn from the stub, and the witness saw it connect to the stub under that pid"
        ),
        ok(runok),
    );
    t.check(
        "the owner's ~/.codex untouched: the decoy $HOME/.codex is as made (its control did change), no Codex process holds a file under ~/.codex",
        ok(home_ok),
    );
    t.check(
        &format!(
            "no kernel refusal names ~/.codex{}",
            if refused == Verdict::Unproven {
                ": the log dropped its control refusal, so its silence shows nothing"
            } else {
                ""
            }
        ),
        refused,
    );
    t.check(crate::recorder::NOTHING_OPENED, ok(open_ok));
    t.check(
        "the isolation probes passed on a replica home with this lab's rules (loopback only, ~/.codex and the owner's data denied, the desktop, the store, the scratch's directories, the lab's state dir)",
        ok(iso == 0),
    );
    t.check(
        "twin refused the scratch's home/Library made a link to a decoy from inside the jail, and wrote nothing through it",
        ok(twin_ok),
    );
    t.check(
        &format!(
            "an orphan in the jail (pid {orphan}: its shell exited, launchd adopted it) was found by the jail's mark and seen by the witness connecting to the stub"
        ),
        ok(orphan_ok),
    );
    Ok(Before {
        tally: t,
        before,
        pids,
        orphan,
    })
}
