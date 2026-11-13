// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The proof for agy in the lab.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::LAB;
use crate::agy::{HOME, Launch, NAME};
use crate::codex_proof::{Tally, Verdict};
use crate::door::Door;
use crate::lab::UpOpts;
use crate::procs::JailOut;
use crate::run::Run;
use crate::selftest::{
    Watched, jail_left, jail_orphan, lab_roots, members_table, observe_open_files, orphan_seen,
    owner_strays, strays_verdict,
};
use crate::sys::{PutSrc, put};
use crate::util::scratch_text;
use crate::{Fail, Res, stop};

/// The decoy file of the scratch shell home's `.gemini` (`up` writes it).
pub const DECOY: &str = "home/.gemini/LAB-DECOY";

/// The reply the selftest's turn is answered with.
const REPLY: &str = "Hi! I am the local lab stub, not a model. (the selftest turn, held 3 s)";

const fn ok(b: bool) -> Verdict {
    if b { Verdict::Ok } else { Verdict::Fail }
}

/// The row that shows agy's composer is up (no trust, sign-in or
/// onboarding screen: the seeds keep them away).
const COMPOSER_ROW: &str = "\\? for shortcuts";

/// The Gemini API requests in the stub's log, as `(how many, the auth labels that are not the lab's fake key)`.
#[must_use]
pub fn gemini_auths(stub_log: &str) -> (usize, Vec<String>) {
    let mut n = 0;
    let mut other = Vec::new();
    for l in stub_log.lines() {
        let Some((_, kind)) = l.split_once(" kind=gemini") else {
            continue;
        };
        n += 1;
        let auth = kind
            .split_whitespace()
            .find_map(|w| w.strip_prefix("auth="))
            .unwrap_or("none");
        if auth != "gemini-key" {
            other.push(auth.to_string());
        }
    }
    (n, other)
}

/// The key check: at least one Gemini API request reached the stub, and every one carried the
/// lab's fake key and nothing else.
#[must_use]
pub fn key_verdict(stub_log: &str) -> Verdict {
    let (n, other) = gemini_auths(stub_log);
    ok(n > 0 && other.is_empty())
}

/// The hosts the black-hole proxy refused, each once in the order first asked. A nested
/// scheme in the authority names none; URLs in the path or query do not replace the host.
#[must_use]
pub fn proxy_refused(stub_log: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for l in stub_log.lines() {
        let Some((_, rest)) = l.split_once("PROXY refused: ") else {
            continue;
        };
        let target = rest
            .split_whitespace()
            .find(|w| w.contains(':') || w.contains('.'))
            .unwrap_or("");
        let authority = target
            .strip_prefix("http://")
            .or_else(|| target.strip_prefix("https://"))
            .unwrap_or(target);
        if authority
            .split_once("://")
            .is_some_and(|(prefix, _)| !prefix.contains(['/', '?', '#']))
        {
            continue;
        }
        let host = authority.split(['/', ':', '?', '#']).next().unwrap_or("");
        if !host.is_empty() && !hosts.iter().any(|h| h == host) {
            hosts.push(host.to_string());
        }
    }
    hosts
}

/// agy's proxy check: the black-hole proxy refused a Google host agy asked for; agy asks for
/// its flag and telemetry hosts on every run, so none asked is unproven.
#[must_use]
pub const fn proxy_verdict(refused: &[String]) -> Verdict {
    if refused.is_empty() {
        Verdict::Unproven
    } else {
        Verdict::Ok
    }
}

/// Whether the egress witness logged one of `pids` connecting to the stub (`cmd[pid] TCP conn 127.0.0.1:<port>`).
#[must_use]
pub fn talked(sockets_log: &str, pids: &[i32], port: &str) -> bool {
    let tail = format!("] TCP conn 127.0.0.1:{port}");
    sockets_log.lines().any(|l| {
        l.strip_suffix(&tail)
            .and_then(|head| head.rsplit_once('['))
            .and_then(|(_, pid)| pid.parse::<i32>().ok())
            .is_some_and(|p| pids.contains(&p))
    })
}

/// The egress check: the witness saw one of the agent's `pids` connect to the stub on `port`,
/// and no `EXTERNAL` line (a socket to a host not loopback) is in either log.
#[must_use]
pub fn egress_verdict(sockets_log: &str, egress_log: &str, pids: &[i32], port: &str) -> Verdict {
    let external = egress_log.lines().any(|l| l.contains("EXTERNAL"))
        || sockets_log.lines().any(|l| l.ends_with("  EXTERNAL"));
    ok(talked(sockets_log, pids, port) && !external)
}

/// The kernel-refusal check: a refusal naming `~/.gemini` (`denied`) fails it; with none, it
/// holds when the log kept the lab's control refusal (`control`), else it is unproven.
#[must_use]
pub const fn refusals_verdict(denied: &[String], control: bool) -> Verdict {
    if !denied.is_empty() {
        Verdict::Fail
    } else if control {
        Verdict::Ok
    } else {
        Verdict::Unproven
    }
}

/// The owner's-`~/.gemini` check: the decoy changed, a file under `~/.gemini` an agent process
/// holds (`held`), no agent process seen, or no write of its own under its scratch HOME
/// (`wrote`) fails it; otherwise it holds when every open-file report was complete
/// (`observed`), else it is unproven.
#[must_use]
pub const fn owner_verdict(
    decoy_kept: bool,
    held: &[String],
    pids: &[i32],
    wrote: &[String],
    observed: bool,
) -> Verdict {
    if !decoy_kept || !held.is_empty() || pids.is_empty() || wrote.is_empty() {
        Verdict::Fail
    } else if observed {
        Verdict::Ok
    } else {
        Verdict::Unproven
    }
}

/// The seeds the lab writes into agy's `.gemini` ([`crate::agy::seed`]),
/// relative to it: what agy itself wrote is everything else.
const SEEDS: &[&str] = &[
    "antigravity-cli",
    "antigravity-cli/settings.json",
    "antigravity-cli/cache",
    "antigravity-cli/cache/onboarding.json",
];

/// The entries under agy's scratch `.gemini` that are none of its seeds.
#[must_use]
pub fn own_writes(rel: &[String]) -> Vec<String> {
    rel.iter()
        .filter(|r| !SEEDS.contains(&r.as_str()))
        .cloned()
        .collect()
}

/// Every entry under `dir`, relative to it (a link is not followed).
fn entries(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            entries(&e.path(), &format!("{name}/"), out);
        }
        out.push(name);
    }
}

/// The decoy's state: its directory's entries with their kind, size and
/// time, and the decoy file's bytes.
fn decoy_state(run: &Run) -> String {
    let d = run.w.join("home/.gemini");
    let mut rows: Vec<String> = fs::read_dir(&d)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let m = e.metadata().ok();
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
    rows.push(scratch_text(&run.w.join(DECOY)));
    rows.join("\n")
}

/// agy's processes in this lab: those that run its binary.
fn agent_pids(run: &Run) -> Vec<i32> {
    let bin = run.agy_bin.clone();
    crate::procs::lab_tree(run)
        .into_iter()
        .filter(|&p| crate::procs::command_line(p).contains(&bin))
        .collect()
}

/// A custom mode the stub answers once, holding the turn 3 s so a 1 Hz
/// witness sees the connection open.
fn held_turn(run: &Run) -> Result<(), Fail> {
    let body =
        format!("{{\"times\": 1, \"delay_ms\": 3000, \"stream\": true, \"text\": \"{REPLY}\"}}\n");
    put(
        &run.w.join("modes/selftest.json"),
        0o644,
        &PutSrc::Bytes(body.as_bytes()),
    )
    .map_err(|e| Fail::die(format!("cannot write the selftest mode: {e}")))?;
    crate::verbs::set_mode(run, "selftest")?;
    Ok(())
}

/// `agy-selftest [tag] [--keep]`.
pub fn cmd_agy_selftest(args: &[String]) -> Res {
    let verb = format!("{NAME}-selftest");
    let (mut tag, mut keep) = (verb.clone(), false);
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
            Fail::die(format!("{verb}: up failed: {}", f.msg.unwrap_or_default()))
        }
    })?;
    let mut t = match checks(&run) {
        Ok(t) => t,
        Err(f) => {
            let _ = crate::lab::down(&tag, false);
            return Err(f);
        }
    };
    if keep {
        println!("{verb}: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let k = crate::lab::down(&tag, false)?;
        println!("lab '{tag}' is down; logs kept in {}", k.display());
        let mut asked = t.before.clone();
        asked.extend(&t.pids);
        let left = jail_left(&run.mark, &asked);
        let pid_list: Vec<String> = t.pids.iter().map(ToString::to_string).collect();
        t.tally.check(
            &format!(
                "nothing in the lab's jail is alive after down, asked of the kernel by its mark (before it: the \
                 tab's {} {}, the orphan {}, {} processes in all){}",
                NAME,
                if pid_list.is_empty() {
                    "none".into()
                } else {
                    pid_list.join(" ")
                },
                t.orphan,
                t.before.len(),
                if left.is_empty() {
                    String::new()
                } else {
                    format!(": alive {left}")
                }
            ),
            ok(t.before.contains(&t.orphan) && !t.pids.is_empty() && left.is_empty()),
        );
    }
    t.tally.verdict(&verb)
}

/// What the checks before `down` leave for the last one.
struct Before {
    tally: Tally,
    before: Vec<i32>,
    pids: Vec<i32>,
    orphan: i32,
}

#[allow(clippy::too_many_lines)]
fn checks(run: &Run) -> Result<Before, Fail> {
    let verb = format!("{NAME}-selftest");
    crate::agy::need(run)?;
    let watched = Watched::of(crate::paths::paths());
    let decoy0 = decoy_state(run);
    let ctlname = format!(
        "lab-control-refusal-{}-{}",
        std::process::id(),
        crate::util::now_secs()
    );
    let _ = crate::procs::jailed(
        run,
        &run.w,
        &[],
        &["/usr/bin/touch", &run.run.join(&ctlname).to_string_lossy()],
        JailOut::Capture,
    );
    let open_control = crate::recorder::control(run);
    println!(
        "== {NAME} {} in lab '{}' (the stub's Gemini API at http://127.0.0.1:{}, a fake GEMINI_API_KEY, the black-hole proxy)",
        run.agy_ver, run.tag, run.port,
    );
    crate::agy::launch(run, &Launch::default()).map_err(|f| {
        Fail::die(format!(
            "{verb}: could not launch {}: {}",
            NAME,
            f.msg.unwrap_or_default()
        ))
    })?;
    let run = &Run::load(&run.tag)?;
    let door = Door::new(run)?;
    let drew = door.seen(COMPOSER_ROW, 60)?;
    let mut answered = false;
    if drew {
        held_turn(run)?;
        answered = crate::verbs::turn(&door, "hi")?
            && door.seen("the selftest turn, held 3 s", 30)?
            && scratch_text(&run.w.join("stub.log")).lines().any(|l| {
                l.contains(":streamGenerateContent -> 200 ")
                    && l.contains("mode=selftest kind=gemini auth=gemini-key")
            });
    }
    let pids = agent_pids(run);
    let sockets =
        || crate::util::read_regular_text(&run.run.join("sockets.log")).unwrap_or_default();
    for _ in 0..20 {
        if talked(&sockets(), &pids, &run.port) {
            break;
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    let stub_log = scratch_text(&run.w.join("stub.log"));
    println!("== stub log");
    for l in stub_log.lines() {
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
        "== sockets of the lab's {} processes (the witness's lines for them)",
        NAME
    );
    for l in sockets()
        .lines()
        .filter(|l| pids.iter().any(|p| l.contains(&format!("[{p}]"))))
    {
        println!("  {l}");
    }
    let opened = crate::recorder::Witness::read(run, &open_control);
    opened.print();
    let mut klog = String::new();
    let mut control = false;
    for _ in 0..3 {
        klog = crate::verbs::sandbox_log("3");
        if klog.contains(&ctlname) {
            control = true;
            break;
        }
        stop::sleep(Duration::from_secs(1))?;
    }
    let pat: Vec<String> = crate::procs::lab_pids_ever(run)
        .iter()
        .map(ToString::to_string)
        .collect();
    let owner_gemini = watched.dirs(".gemini");
    let denied: Vec<String> = klog
        .lines()
        .filter_map(|l| crate::verbs::refusal_of(l, &pat))
        .filter(|l| owner_gemini.iter().any(|g| l.contains(g.as_str())))
        .take(5)
        .collect();
    let roots = lab_roots(run, &run.agy_bin, watched.homes());
    let (mut held, mut strays, mut observed) = (Vec::new(), Vec::new(), true);
    for &p in &pids {
        let seen = observe_open_files(p);
        observed &= seen.complete;
        held.extend(
            seen.text
                .lines()
                .filter(|l| owner_gemini.iter().any(|g| l.starts_with(&format!("n{g}"))))
                .map(str::to_string),
        );
        held.truncate(5);
        strays.extend(owner_strays(&seen.text, watched.homes(), &roots));
        strays.truncate(5);
    }
    let decoy1 = decoy_state(run);
    let mut rel = Vec::new();
    entries(&run.w.join(HOME).join(".gemini"), "", &mut rel);
    rel.sort();
    let wrote = own_writes(&rel);
    println!(
        "== the owner's ~/.gemini (never opened here: the kernel's refusals naming it, the agent's open files under it, \
         the scratch shell home's decoy)"
    );
    println!(
        "  kernel refusals naming it: {} (the log {} this lab's control refusal, a jailed write into the run directory)",
        if denied.is_empty() {
            "none".into()
        } else {
            denied.join(" | ")
        },
        if control { "kept" } else { "dropped" }
    );
    let pid_list: Vec<String> = pids.iter().map(ToString::to_string).collect();
    println!(
        "  open under it, by the lab's {} (pids {}): {}",
        NAME,
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
        "  any other file of the owner's home it holds, outside the run directory and its build: {}",
        if !strays.is_empty() {
            strays.join(" ")
        } else if !observed {
            "UNPROVEN: an open-file observer did not return a complete successful report".into()
        } else {
            "none".into()
        }
    );
    println!(
        "  the decoy {}; its control: the agent wrote {} entries of its own under the scratch HOME it was given \
         ({}/.gemini: {})",
        if decoy0 == decoy1 {
            "is unchanged (only LAB-DECOY)"
        } else {
            "CHANGED"
        },
        wrote.len(),
        HOME,
        wrote.iter().take(6).cloned().collect::<Vec<_>>().join(" ")
    );
    println!(
        "== isolation probes (the rules of this lab's jail rendered for a replica home: nothing of the owner's is \
         touched)"
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

    let egress = crate::util::read_regular_text(&run.run.join("egress.log")).unwrap_or_default();
    let (requests, other_auth) = gemini_auths(&stub_log);
    let refused = proxy_refused(&stub_log);
    let refusals = refusals_verdict(&denied, control);
    let open_ok = opened.ok();
    println!("== checks");
    let mut t = Tally::default();
    t.check(
        &format!(
            "{} {} started in the tab and drew its composer (no sign-in, trust or onboarding screen)",
            NAME,
            run.agy_ver
        ),
        ok(drew),
    );
    t.check(
        "the turn \"hi\" was answered by the stub (a kind=gemini auth=gemini-key 200 in its log) and the TUI shows its \
         reply",
        ok(answered),
    );
    t.check(
        &format!(
            "no non-loopback socket was seen, and the egress witness did see the tab's {} connect to the stub (port {})",
            NAME,
            run.port
        ),
        egress_verdict(&sockets(), &egress, &pids, &run.port),
    );
    t.check(
        &format!(
            "every Gemini API request the stub took ({requests}) carried the lab's fake key, never a Google login{}",
            if other_auth.is_empty() {
                String::new()
            } else {
                format!(": {}", other_auth.join(" "))
            }
        ),
        key_verdict(&stub_log),
    );
    t.check(
        &format!(
            "agy asked for Google's hosts only through the black-hole proxy, which refused every one ({})",
            if refused.is_empty() {
                "none asked this run, so this shows nothing".into()
            } else {
                refused.join(" ")
            }
        ),
        proxy_verdict(&refused),
    );
    t.check(
        &format!(
            "the owner's ~/.gemini untouched: the decoy $HOME/.gemini of the scratch's shell home is as made, no {} \
             process holds a file under ~/.gemini, and the agent wrote under its own scratch HOME (its control)",
            NAME
        ),
        owner_verdict(decoy0 == decoy1, &held, &pids, &wrote, observed),
    );
    t.check(
        &format!(
            "no {} process holds a file of the owner's home outside the run directory and its own build (a file held \
             there is a gap in the jail's protected list)",
            NAME
        ),
        strays_verdict(&strays, &pids, observed),
    );
    t.check(
        &format!(
            "no kernel refusal names ~/.gemini{}",
            if refusals == Verdict::Unproven {
                ": the log dropped its control refusal, so its silence shows nothing"
            } else {
                ""
            }
        ),
        refusals,
    );
    t.check(crate::recorder::NOTHING_OPENED, ok(open_ok));
    t.check(
        "the isolation probes passed on a replica home with this lab's rules (loopback only, ~/.gemini and the \
         owner's data denied, the desktop, the store, the scratch's directories, the lab's state dir)",
        ok(iso == 0),
    );
    t.check(
        "twin refused the scratch's home/Library made a link to a decoy from inside the jail, and wrote nothing \
         through it",
        ok(twin_ok),
    );
    t.check(
        &format!(
            "an orphan in the jail (pid {orphan}: its shell exited, launchd adopted it) was found by the jail's mark \
             and seen by the witness connecting to the stub"
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

#[cfg(test)]
#[path = "agy_proof_tests.rs"]
mod tests;
