// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The proofs for Gemini CLI and agy in the lab.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::LAB;
use crate::codex_proof::{Tally, Verdict};
use crate::door::Door;
use crate::gemini::{Agent, Launch};
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

/// The row that shows the agent's composer is up (no trust, sign-in or
/// onboarding screen: the seeds keep them away).
const fn composer_row(agent: Agent) -> &'static str {
    match agent {
        Agent::Gemini => "Type your message",
        Agent::Agy => "\\? for shortcuts",
    }
}

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

/// The hosts the black-hole proxy refused (`PROXY refused: CONNECT <host>:<port> …` in the stub's log), each once, in the order first asked.
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
        let host = target
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split(['/', ':'])
            .next()
            .unwrap_or("");
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

/// The seeds the lab writes into the agent's `.gemini` ([`crate::gemini::seed`]),
/// relative to it: what the agent itself wrote is everything else.
const fn seeds(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Gemini => &["settings.json", "trustedFolders.json"],
        Agent::Agy => &[
            "antigravity-cli",
            "antigravity-cli/settings.json",
            "antigravity-cli/cache",
            "antigravity-cli/cache/onboarding.json",
        ],
    }
}

/// The entries under the agent's scratch `.gemini` that are none of its seeds.
#[must_use]
pub fn own_writes(agent: Agent, rel: &[String]) -> Vec<String> {
    rel.iter()
        .filter(|r| !seeds(agent).contains(&r.as_str()))
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
    rows.push(scratch_text(&run.w.join(DECOY)));
    rows.join("\n")
}

/// The agent's processes in this lab: those that run its binary.
fn agent_pids(run: &Run, agent: Agent) -> Vec<i32> {
    let bin = agent.bin(run).to_string();
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

/// `gemini-selftest [tag] [--keep]`.
pub fn cmd_gemini_selftest(args: &[String]) -> Res {
    cmd(Agent::Gemini, args)
}

/// `agy-selftest [tag] [--keep]`.
pub fn cmd_agy_selftest(args: &[String]) -> Res {
    cmd(Agent::Agy, args)
}

fn cmd(agent: Agent, args: &[String]) -> Res {
    let verb = format!("{}-selftest", agent.name());
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
    let mut t = match checks(agent, &run) {
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
                agent.name(),
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
fn checks(agent: Agent, run: &Run) -> Result<Before, Fail> {
    let verb = format!("{}-selftest", agent.name());
    crate::gemini::need(run, agent)?;
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
        "== {} {} in lab '{}' (the stub's Gemini API at http://127.0.0.1:{}, a fake GEMINI_API_KEY{})",
        agent.name(),
        agent.ver(run),
        run.tag,
        run.port,
        match agent {
            Agent::Gemini => ", no proxy: the jail alone",
            Agent::Agy => ", the black-hole proxy",
        }
    );
    crate::gemini::launch(run, agent, &Launch::default()).map_err(|f| {
        Fail::die(format!(
            "{verb}: could not launch {}: {}",
            agent.name(),
            f.msg.unwrap_or_default()
        ))
    })?;
    let run = &Run::load(&run.tag)?;
    let door = Door::new(run)?;
    let drew = door.seen(composer_row(agent), 60)?;
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
    let pids = agent_pids(run, agent);
    let sockets = || fs::read_to_string(run.run.join("sockets.log")).unwrap_or_default();
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
        agent.name()
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
    let roots = lab_roots(run, agent.bin(run), watched.homes());
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
    entries(&run.w.join(agent.home()).join(".gemini"), "", &mut rel);
    rel.sort();
    let wrote = own_writes(agent, &rel);
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
        agent.name(),
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
        agent.home(),
        wrote.iter().take(6).cloned().collect::<Vec<_>>().join(" ")
    );
    println!(
        "== isolation probes (the rules of this lab's jail rendered for a replica home: nothing of the owner's is \
         touched)"
    );
    let (iso, probe_text) = crate::probe::run_captured(&run.tag, true)?;
    let _ = fs::write(run.run.join("probe.txt"), &probe_text);
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

    let egress = fs::read_to_string(run.run.join("egress.log")).unwrap_or_default();
    let (requests, other_auth) = gemini_auths(&stub_log);
    let refused = proxy_refused(&stub_log);
    let refusals = refusals_verdict(&denied, control);
    let open_ok = opened.ok();
    println!("== checks");
    let mut t = Tally::default();
    t.check(
        &format!(
            "{} {} started in the tab and drew its composer (no sign-in, trust or onboarding screen)",
            agent.name(),
            agent.ver(run)
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
            agent.name(),
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
    if agent == Agent::Agy {
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
    }
    t.check(
        &format!(
            "the owner's ~/.gemini untouched: the decoy $HOME/.gemini of the scratch's shell home is as made, no {} \
             process holds a file under ~/.gemini, and the agent wrote under its own scratch HOME (its control)",
            agent.name()
        ),
        owner_verdict(decoy0 == decoy1, &held, &pids, &wrote, observed),
    );
    t.check(
        &format!(
            "no {} process holds a file of the owner's home outside the run directory and its own build (a file held \
             there is a gap in the jail's protected list)",
            agent.name()
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
mod tests {
    use super::*;

    const STUB_LOG: &str = "\
13:02:46.080 stub listening on 127.0.0.1:52886 (proxy black hole: 52887) mode=ok pid=38489
13:02:46.716 PROXY refused: CONNECT antigravity-unleash.goog:443 HTTP/1.1
13:02:46.716 PROXY refused: CONNECT antigravity-unleash.goog:443 HTTP/1.1
13:02:47.103 POST /v1beta/models/m:streamGenerateContent -> 200 body=738B mode=ok kind=gemini-aux auth=gemini-key model=m
13:02:47.113 POST /v1beta/models/m:streamGenerateContent -> 200 body=53718B mode=ok kind=gemini auth=gemini-key model=m
13:02:51.728 PROXY refused: CONNECT play.googleapis.com:443 HTTP/1.1
13:02:52.000 POST /v1beta/models/m:countTokens -> 200 body=10B mode=ok kind=gemini-count auth=gemini-key tokens=3
";

    /// The stub's log says every Gemini API request carried the fake key.
    #[test]
    fn every_gemini_request_names_the_key_it_carried() {
        assert_eq!(gemini_auths(STUB_LOG), (3, Vec::new()));
        let login = format!(
            "{STUB_LOG}13:03:00.000 POST /v1beta/models/m:streamGenerateContent -> 200 mode=ok kind=gemini \
             auth=jwt:owner model=m\n13:03:01.000 POST /v1beta/models/m:generateContent -> 401 mode=ok \
             kind=gemini-json model=m\n"
        );
        assert_eq!(
            gemini_auths(&login),
            (5, vec!["jwt:owner".to_string(), "none".to_string()])
        );
        let claude_only = "13:00:00.000 POST /v1/messages -> 200 mode=ok kind=main auth=key\n";
        assert_eq!(gemini_auths(claude_only), (0, Vec::new()));
    }

    /// THE KEY CHECK FAILS ON ANY CREDENTIAL BUT THE LAB'S KEY ALONE: the
    /// selftest's own log passes it, and so does the catalog's GET under the
    /// key; a Google login — a bearer token beside the key, one alone, a
    /// request with no key, each labelled as the stub labels it
    /// ([`crate::stub::gemini::credential_label`]) — fails it.
    #[test]
    fn the_key_check_fails_on_any_credential_but_the_labs_key_alone() {
        use crate::stub::gemini::credential_label;
        assert_eq!(key_verdict(STUB_LOG), Verdict::Ok);
        let catalog = |label: &str| {
            format!(
                "{STUB_LOG}13:03:00.000 GET /v1beta/models -> 200 body=0B mode=ok kind=gemini-models \
                 auth={label}\n"
            )
        };
        let key = Some(crate::gemini::LAB_GEMINI_KEY);
        assert_eq!(
            key_verdict(&catalog(&credential_label(key, None))),
            Verdict::Ok
        );
        let login = Some("Bearer ya29.a-google-login");
        for label in [
            credential_label(key, login),
            credential_label(None, login),
            credential_label(None, None),
        ] {
            assert_eq!(key_verdict(&catalog(&label)), Verdict::Fail, "{label}");
        }
        let quiet: String = STUB_LOG
            .lines()
            .filter(|l| !l.contains(" kind=gemini"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(key_verdict(&quiet), Verdict::Fail, "the control");
    }

    /// The witness's logs as the sampler writes them ([`crate::sampler`]): each
    /// new endpoint `<ts> <line>` in `sockets.log`, and one whose host is not
    /// loopback `<ts> EXTERNAL <line>` in `egress.log` too.
    fn witness_logs(lsof_f: &str) -> (String, String) {
        let (mut sockets, mut egress) = (String::new(), String::new());
        for line in crate::sampler::endpoints(lsof_f) {
            sockets.push_str(&format!("13:02:47 {line}\n"));
            if line.ends_with("EXTERNAL") {
                egress.push_str(&format!("13:02:47 EXTERNAL {line}\n"));
            }
        }
        (sockets, egress)
    }

    /// THE EGRESS CHECK FAILS WHEN EGRESS HAPPENS: the witness's own lines —
    /// the sampler's reading of `lsof` — for agy talking to the stub and the
    /// proxy pass it; one socket of the lab's to a Google address, agy's own
    /// or another process's, IPv4 or IPv6, fails it, and so does its line in
    /// either log alone.
    #[test]
    fn the_egress_check_fails_when_any_socket_leaves_loopback() {
        let (pids, port) = (&[38564][..], "52886");
        let stub = "p38564\ncagy\nPTCP\nn127.0.0.1:50100->127.0.0.1:52886 (ESTABLISHED)\n\
                    n127.0.0.1:50101->127.0.0.1:52887\n";
        let (sockets, egress) = witness_logs(stub);
        assert!(egress.is_empty(), "{sockets}");
        assert_eq!(egress_verdict(&sockets, &egress, pids, port), Verdict::Ok);

        for leak in [
            "n10.0.0.2:50102->142.250.72.10:443 (SYN_SENT)\n",
            "p38600\ncnode\nPTCP\nn[fd00::2]:50103->[2607:f8b0:4005::200e]:443\n",
        ] {
            let (sockets, egress) = witness_logs(&format!("{stub}{leak}"));
            assert!(!egress.is_empty(), "the witness marks it: {leak}");
            assert_eq!(
                egress_verdict(&sockets, &egress, pids, port),
                Verdict::Fail,
                "{leak}"
            );
            assert_eq!(
                egress_verdict(&sockets, "", pids, port),
                Verdict::Fail,
                "sockets.log alone: {leak}"
            );
            let (clean, _) = witness_logs(stub);
            assert_eq!(
                egress_verdict(&clean, &egress, pids, port),
                Verdict::Fail,
                "egress.log alone: {leak}"
            );
        }

        assert_eq!(
            egress_verdict(&sockets, &egress, &[38565], port),
            Verdict::Fail,
            "the agent never seen"
        );
        let (listen, quiet) = witness_logs("p38564\ncagy\nPTCP\nn127.0.0.1:52886\n");
        assert_eq!(
            egress_verdict(&listen, &quiet, pids, port),
            Verdict::Fail,
            "a listen"
        );
    }

    /// A refusal naming `~/.gemini` fails the refusals check whether or not
    /// the log kept the control; with none, the check holds only on a kept
    /// control.
    #[test]
    fn a_refusal_naming_gemini_fails_whether_or_not_the_control_was_kept() {
        let denied = ["deny(1) file-read-data /Users//owner/.gemini/settings.json".to_string()];
        assert_eq!(refusals_verdict(&denied, true), Verdict::Fail);
        assert_eq!(refusals_verdict(&denied, false), Verdict::Fail);
        assert_eq!(refusals_verdict(&[], true), Verdict::Ok);
        assert_eq!(refusals_verdict(&[], false), Verdict::Unproven);
        assert_eq!(proxy_verdict(&[]), Verdict::Unproven);
        assert_eq!(
            proxy_verdict(&["antigravity-unleash.goog".to_string()]),
            Verdict::Ok
        );
    }

    /// THE OWNER'S-`~/.gemini` CHECK FAILS ON A HELD FILE (a failed report's
    /// included), a changed decoy, no agent process or no write of its own;
    /// otherwise it holds only when every open-file report was complete.
    #[test]
    fn the_owner_gemini_check_fails_on_a_held_file_and_needs_complete_reports() {
        use crate::selftest::open_files;
        use std::os::unix::process::ExitStatusExt as _;
        let held = vec!["n/Users//owner/.gemini/oauth_creds.json".to_string()];
        let wrote = vec!["settings.json.bak".to_string()];
        assert_eq!(
            owner_verdict(true, &held, &[7], &wrote, true),
            Verdict::Fail
        );
        assert_eq!(
            owner_verdict(true, &held, &[7], &wrote, false),
            Verdict::Fail
        );
        assert_eq!(owner_verdict(false, &[], &[7], &wrote, true), Verdict::Fail);
        assert_eq!(owner_verdict(true, &[], &[], &wrote, true), Verdict::Fail);
        assert_eq!(owner_verdict(true, &[], &[7], &[], true), Verdict::Fail);
        assert_eq!(owner_verdict(true, &[], &[7], &wrote, true), Verdict::Ok);
        assert_eq!(
            owner_verdict(true, &[], &[7], &wrote, false),
            Verdict::Unproven
        );
        let out = |code, stdout: &[u8], stderr: &[u8]| {
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(code),
                stdout: stdout.to_vec(),
                stderr: stderr.to_vec(),
            })
        };
        let failed = open_files(
            7,
            out(
                1 << 8,
                b"p7\nn/Users//owner/.gemini/oauth_creds.json\n",
                b"lsof: WARNING",
            ),
        );
        assert!(!failed.complete);
        assert!(
            failed
                .text
                .contains("n/Users//owner/.gemini/oauth_creds.json")
        );
        let clean = crate::selftest::complete_open_files(
            7,
            out(0, b"p7\nn/private/tmp/x\n", b"")
                .map_err(aterm_uds::deadline::CompleteOutputError::Run),
        );
        assert!(clean.complete);
        let timed_out = open_files(7, Err(aterm_uds::deadline::RunError::TimedOut));
        assert!(!timed_out.complete && timed_out.text.is_empty());
    }

    /// The black-hole proxy's refusals name each host once, in the order first
    /// asked.
    #[test]
    fn the_proxy_names_each_refused_host_once() {
        assert_eq!(
            proxy_refused(STUB_LOG),
            ["antigravity-unleash.goog", "play.googleapis.com"]
        );
        let plain = "13:00:00 PROXY refused: GET http://metadata.google.internal/computeMetadata/v1 HTTP/1.1\n";
        assert_eq!(proxy_refused(plain), ["metadata.google.internal"]);
        let quiet: String = STUB_LOG
            .lines()
            .filter(|l| !l.contains("PROXY"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(proxy_refused(&quiet).is_empty());
    }

    /// The witness's line for a pid's connection to the stub, any program's
    /// name.
    #[test]
    fn the_witness_names_the_pid_that_talked_to_the_stub() {
        let log = "13:02:47 agy[38564] TCP conn 127.0.0.1:52886\n\
                   13:02:47 agy[38564] TCP listen 127.0.0.1:52888\n\
                   13:02:48 node[4021] TCP conn 127.0.0.1:52887\n";
        assert!(talked(log, &[38564], "52886"));
        assert!(!talked(log, &[38565], "52886"));
        assert!(!talked(log, &[4021], "52886"), "the proxy's port");
        assert!(!talked(log, &[38564], "52888"), "a listen");
        assert!(!talked(log, &[3856], "52886"));
    }

    /// What the agent wrote under its scratch `.gemini` is everything but the
    /// lab's own seeds.
    #[test]
    fn the_agents_own_writes_are_what_is_not_a_seed() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let agy = v(&[
            "antigravity-cli",
            "antigravity-cli/cache",
            "antigravity-cli/cache/onboarding.json",
            "antigravity-cli/log",
            "antigravity-cli/log/cli-20261016_130246.log",
            "antigravity-cli/settings.json",
        ]);
        assert_eq!(
            own_writes(Agent::Agy, &agy),
            v(&[
                "antigravity-cli/log",
                "antigravity-cli/log/cli-20261016_130246.log"
            ])
        );
        let seeded = v(&[
            "antigravity-cli",
            "antigravity-cli/cache",
            "antigravity-cli/cache/onboarding.json",
            "antigravity-cli/settings.json",
        ]);
        assert!(own_writes(Agent::Agy, &seeded).is_empty(), "the control");
        let gemini = v(&["installation_id", "settings.json", "tmp"]);
        assert_eq!(
            own_writes(Agent::Gemini, &gemini),
            v(&["installation_id", "tmp"])
        );
        assert!(own_writes(Agent::Gemini, &v(&["settings.json"])).is_empty());
    }
}
