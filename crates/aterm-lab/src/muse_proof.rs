// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lab selftest and isolation proof (`muse-selftest`) for Muse Code.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::LAB;
use crate::codex_proof::{Tally, Verdict};
use crate::door::Door;
use crate::gemini_proof::{egress_verdict, refusals_verdict, talked};
use crate::lab::UpOpts;
use crate::muse::Launch;
use crate::procs::JailOut;
use crate::run::Run;
use crate::runtime_dirs::Held;
use crate::selftest::{
    Watched, jail_left, jail_orphan, lab_roots, members_table, observe_open_files, orphan_seen,
    owner_dirs, owner_strays, strays_verdict,
};
use crate::sys::{PutSrc, put};
use crate::util::scratch_text;
use crate::{Fail, Res, stop};

/// The reply the selftest's turn is answered with.
const REPLY: &str = "Hi! I am the local lab stub, not a model. (the selftest turn, held 3 s)";

/// The selftest's turn: Muse Code answers a bare greeting itself, with no
/// request.
const PROMPT: &str = "carry on with the task";

/// Muse Code config and data directories relative to a home directory.
pub const OWN_DIRS: [&str; 2] = [".config/muse", ".local/share/muse"];

/// Owner Muse Code directories relative to the owner's home directory.
pub const OWNER_DIRS: [&str; 3] = [
    ".config/muse",
    ".local/share/muse",
    "Library/Application Support/Muse",
];

fn ok(b: bool) -> Verdict {
    if b { Verdict::Ok } else { Verdict::Fail }
}

/// Extracts Meta route request count and non-lab auth labels from `stub_log`.
#[must_use]
pub fn muse_auths(stub_log: &str) -> (usize, Vec<String>) {
    let mut n = 0;
    let mut other = Vec::new();
    for l in stub_log.lines() {
        let Some((_, kind)) = l.split_once(" kind=muse") else {
            continue;
        };
        n += 1;
        let auth = kind
            .split_whitespace()
            .find_map(|w| w.strip_prefix("auth="))
            .unwrap_or("none");
        if auth != "key" {
            other.push(auth.to_string());
        }
    }
    (n, other)
}

/// Verifies that the stub observed Meta requests using only the lab's fake API key.
#[must_use]
pub fn key_verdict(stub_log: &str) -> Verdict {
    let (n, other) = muse_auths(stub_log);
    ok(n > 0 && other.is_empty())
}

/// Extracts open file paths from `lsof -Fn` output under [`OWNER_DIRS`] for `homes`.
#[must_use]
pub fn held_under(lsof_fn: &str, homes: &[String]) -> Vec<String> {
    let roots: Vec<String> = OWNER_DIRS
        .iter()
        .flat_map(|d| owner_dirs(homes, d))
        .collect();
    lsof_fn
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter(|path| {
            roots.iter().any(|r| {
                path.strip_prefix(r.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
        })
        .map(str::to_string)
        .collect()
}

/// Filters kernel sandbox refusal lines naming [`OWNER_DIRS`] under `homes`.
#[must_use]
pub fn naming_own_dirs(refusals: &[String], homes: &[String]) -> Vec<String> {
    let roots: Vec<String> = OWNER_DIRS
        .iter()
        .flat_map(|d| owner_dirs(homes, d))
        .collect();
    refusals
        .iter()
        .filter(|l| {
            roots.iter().any(|r| {
                l.match_indices(r.as_str()).any(|(at, _)| {
                    l[at + r.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| c == '/' || c.is_whitespace() || c == '"' || c == ')')
                })
            })
        })
        .cloned()
        .collect()
}

/// Verifies that no Muse Code process held files under the owner's directories.
#[must_use]
pub fn owner_verdict(held: &[String], wrote: &[String], pids: &[i32], observed: bool) -> Verdict {
    if !held.is_empty() {
        Verdict::Fail
    } else if !observed || pids.is_empty() || wrote.is_empty() {
        Verdict::Unproven
    } else {
        Verdict::Ok
    }
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

/// What Muse Code wrote under the scratch HOME's own directories ([`OWN_DIRS`]
/// under `<scratch>/home`), relative to that home: the lab seeds nothing
/// there.
fn own_writes(run: &Run) -> Vec<String> {
    let home = run.w.join("home");
    let mut out = Vec::new();
    for d in OWN_DIRS {
        entries(&home.join(d), &format!("{d}/"), &mut out);
    }
    out.sort();
    out
}

/// Muse Code's processes in this lab: those that run its binary.
fn muse_pids(run: &Run) -> Vec<i32> {
    crate::procs::lab_tree(run)
        .into_iter()
        .filter(|&p| crate::procs::command_line(p).contains(&run.muse_bin))
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

/// Runs `muse-selftest [tag] [--keep]`.
pub fn cmd_muse_selftest(args: &[String]) -> Res {
    let verb = "muse-selftest";
    let (mut tag, mut keep) = (verb.to_string(), false);
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
    let registry = crate::runtime_dirs::roots(crate::sys::getuid());
    let listed = crate::runtime_dirs::listing(&registry);
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
                 tab's Muse Code {}, the orphan {}, {} processes in all){}",
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
        let made: Vec<String> = t.runtime.iter().map(|h| h.path.clone()).collect();
        let since = crate::runtime_dirs::after_down(
            &listed,
            &crate::runtime_dirs::listing(&registry),
            &t.runtime,
            &registry,
            &crate::runtime_dirs::Disk,
            &|p| crate::sys::alive(p) && !asked.contains(&p),
        );
        let left = &since.left;
        let list = |v: &[String]| {
            if v.is_empty() {
                "none".to_string()
            } else {
                v.join(" ")
            }
        };
        println!(
            "  new in {} since up, naming a live Muse Code outside the jail: {}; the registry's shared structure a \
             first run makes: {}",
            registry.join(" "),
            list(&since.theirs),
            list(&since.shared)
        );
        t.tally.check(
            &format!(
                "nothing the tab's Muse Code made in the shared {} is left after down: a listing before up and after \
                 down differ only by entries naming a live Muse Code outside the jail and the registry's shared \
                 directories and lock, which down leaves ({} made: its lease, socket and session record are the \
                 control){}",
                registry.join(" "),
                made.len(),
                if left.is_empty() {
                    String::new()
                } else {
                    format!(": left {}", left.join(" "))
                }
            ),
            runtime_verdict(&made, left, since.complete),
        );
    }
    t.tally.verdict(verb)
}

/// What the checks before `down` leave for the last ones.
struct Before {
    tally: Tally,
    before: Vec<i32>,
    pids: Vec<i32>,
    orphan: i32,
    /// What the tab's Muse Code made in its shared runtime directory.
    runtime: Vec<Held>,
}

/// Verifies that Muse Code left no residual files in the shared runtime directory after shutdown.
#[must_use]
pub fn runtime_verdict(made: &[String], left: &[String], complete: bool) -> Verdict {
    let has = |end: &str, inside: &str| made.iter().any(|p| p.ends_with(end) && p.contains(inside));
    if !left.is_empty() {
        Verdict::Fail
    } else if complete
        && has(".sock.lease", "/ms-")
        && has(".sock", "/ms-")
        && has(".json", "/sessions/")
    {
        Verdict::Ok
    } else {
        Verdict::Unproven
    }
}

#[allow(clippy::too_many_lines)]
fn checks(run: &Run) -> Result<Before, Fail> {
    let verb = "muse-selftest";
    crate::muse::need_muse(run)?;
    let watched = Watched::of(crate::paths::paths());
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
        "== Muse Code {} in lab '{}' (the stub's Meta model API at http://127.0.0.1:{}, a fake META_API_KEY, every \
         proxy the black hole)",
        run.muse_ver, run.tag, run.port
    );
    let l = Launch {
        args: vec!["--trust-workspace".into()],
        ..Launch::default()
    };
    crate::muse::launch(run, &l).map_err(|f| {
        Fail::die(format!(
            "{verb}: could not launch Muse Code: {}",
            f.msg.unwrap_or_default()
        ))
    })?;
    let run = &Run::load(&run.tag)?;
    let door = Door::new(run)?;
    let drew = door.seen("Voice input", 60)?;
    let mut answered = false;
    if drew {
        held_turn(run)?;
        answered = crate::verbs::turn(&door, PROMPT)?
            && door.seen("the selftest turn, held 3 s", 30)?
            && scratch_text(&run.w.join("stub.log")).lines().any(|l| {
                l.contains("POST /responses -> 200 ")
                    && l.contains("mode=selftest kind=muse auth=key")
            });
    }
    let pids = muse_pids(run);
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
    println!("== sockets of the lab's Muse Code processes (the witness's lines for them)");
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
    let refusals: Vec<String> = klog
        .lines()
        .filter_map(|l| crate::verbs::refusal_of(l, &pat))
        .collect();
    let denied: Vec<String> = naming_own_dirs(&refusals, watched.homes())
        .into_iter()
        .take(5)
        .collect();
    let (mut held, mut strays, mut observed) = (Vec::new(), Vec::new(), true);
    let roots = lab_roots(run, &run.muse_bin, watched.homes());
    for &pid in &pids {
        let seen = observe_open_files(pid);
        observed &= seen.complete;
        held.extend(held_under(&seen.text, watched.homes()));
        held.truncate(5);
        strays.extend(owner_strays(&seen.text, watched.homes(), &roots));
        strays.truncate(5);
    }
    let wrote = own_writes(run);
    println!(
        "== the owner's ~/.config/muse, ~/.local/share/muse and ~/Library/Application Support/Muse (never opened \
         here: the kernel's refusals naming them, Muse Code's open files under them, its own writes under the scratch \
         HOME)"
    );
    println!(
        "  kernel refusals naming them: {} (the log {} this lab's control refusal, a jailed write into the run \
         directory)",
        if denied.is_empty() {
            "none".into()
        } else {
            denied.join(" | ")
        },
        if control { "kept" } else { "dropped" }
    );
    let pid_list: Vec<String> = pids.iter().map(ToString::to_string).collect();
    println!(
        "  open under them, by the lab's Muse Code (pids {}): {}",
        if pid_list.is_empty() {
            "none".into()
        } else {
            pid_list.join(" ")
        },
        if !held.is_empty() {
            held.join(" ")
        } else if !observed {
            "UNPROVEN: an open-file observer did not return a complete successful report".into()
        } else {
            "none".into()
        }
    );
    println!(
        "  its control: Muse Code wrote {} entries under the scratch HOME it was given ({})",
        wrote.len(),
        wrote.iter().take(6).cloned().collect::<Vec<_>>().join(" ")
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
    let runtime: Vec<Held> = crate::runtime_dirs::claimed(&run.run)
        .into_iter()
        .filter(|h| pids.contains(&h.pid))
        .collect();
    println!(
        "== the shared runtime directory {} (what the tab's Muse Code made there, which down takes back)",
        crate::runtime_dirs::roots(crate::sys::getuid()).join(" ")
    );
    for h in &runtime {
        println!("  pid {} holds {}", h.pid, h.path);
    }
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
    let before = crate::sys::jail_pids(&run.mark).unwrap_or_default();
    println!(
        "== the jail's members, by its mark (pid, ppid, command; the orphan control is {orphan}, adopted by launchd)"
    );
    print!("{}", members_table(&before));

    let egress = fs::read_to_string(run.run.join("egress.log")).unwrap_or_default();
    let (requests, other_auth) = muse_auths(&stub_log);
    let refusals = refusals_verdict(&denied, control);
    let open_ok = opened.ok();
    println!("== checks");
    let mut t = Tally::default();
    t.check(
        &format!(
            "Muse Code {} started in the tab and drew its composer (no sign-in or trust screen)",
            run.muse_ver
        ),
        ok(drew),
    );
    t.check(
        "the turn was answered by the stub (a kind=muse auth=key 200 in its log) and the TUI shows its reply",
        ok(answered),
    );
    t.check(
        &format!(
            "no non-loopback socket was seen, and the egress witness did see the tab's Muse Code connect to the \
             stub (port {})",
            run.port
        ),
        egress_verdict(&sockets(), &egress, &pids, &run.port),
    );
    t.check(
        &format!(
            "every request to the stub's Meta routes ({requests}) carried the lab's fake key, never a Meta login{}",
            if other_auth.is_empty() {
                String::new()
            } else {
                format!(": {}", other_auth.join(" "))
            }
        ),
        key_verdict(&stub_log),
    );
    t.check(
        "the owner's ~/.config/muse, ~/.local/share/muse and ~/Library/Application Support/Muse untouched: no Muse \
         Code process holds a file under them, and Muse Code wrote under its own scratch HOME (its control)",
        owner_verdict(&held, &wrote, &pids, observed),
    );
    t.check(
        "no Muse Code process holds a file of the owner's home outside the run directory and its own build (a file \
         held there is a gap in the jail's protected list)",
        strays_verdict(&strays, &pids, observed),
    );
    t.check(
        &format!(
            "no kernel refusal names ~/.config/muse, ~/.local/share/muse or ~/Library/Application Support/Muse{}",
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
        "the isolation probes passed on a replica home with this lab's rules (loopback only, the owner's data \
         denied, the desktop, the store, the scratch's directories, the lab's state dir)",
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
        runtime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUB_LOG: &str = "\
18:13:10.100 stub listening on 127.0.0.1:50581 (proxy black hole: 50582) mode=ok pid=46492
18:13:10.820 GET /muse-code/models -> 200 body=0B mode=ok kind=muse-models auth=key models=1
18:13:26.349 POST /responses -> 200 body=133593B mode=selftest kind=muse auth=key model=lab-stub stream=true items=2
18:13:26.349 POST /responses -> 200 body=65500B mode=selftest kind=muse-side role=reminder:skill-reminder auth=key model=lab-stub
18:13:30.870 POST /responses -> 200 body=2235B mode=ok kind=muse-side role=prompt-hint auth=key model=lab-stub
18:13:31.000 PROXY refused: CONNECT graph.facebook.com:443 HTTP/1.1
";

    /// Every request to the stub's Meta routes names the key it carried: the
    /// turn, its side calls and the catalog.
    #[test]
    fn every_meta_request_names_the_key_it_carried() {
        assert_eq!(muse_auths(STUB_LOG), (4, Vec::new()));
        let login = format!(
            "{STUB_LOG}18:14:00.000 POST /responses -> 200 mode=ok kind=muse auth=jwt:owner model=m\n\
             18:14:01.000 POST /responses -> 401 mode=ok kind=muse-side role=judge model=m\n"
        );
        assert_eq!(
            muse_auths(&login),
            (6, vec!["jwt:owner".to_string(), "none".to_string()])
        );
        let claude_only = "13:00:00.000 POST /v1/messages -> 200 mode=ok kind=main auth=key\n";
        assert_eq!(muse_auths(claude_only), (0, Vec::new()));
    }

    /// THE KEY CHECK FAILS ON ANY CREDENTIAL BUT THE LAB'S KEY: the
    /// selftest's own log passes it; a Meta login — a bearer token of
    /// another shape, a request with none, each labelled as the stub labels
    /// it ([`crate::stub::auth_label`]) — fails it.
    #[test]
    fn the_key_check_fails_on_any_credential_but_the_labs_key() {
        use crate::stub::auth_label;
        assert_eq!(key_verdict(STUB_LOG), Verdict::Ok);
        let with = |label: &str| {
            format!(
                "{STUB_LOG}18:14:00.000 POST /responses -> 200 mode=ok kind=muse auth={label}\n"
            )
        };
        let key = format!("Bearer {}", crate::stub::LAB_CODEX_KEY);
        assert_eq!(key_verdict(&with(&auth_label(Some(&key)))), Verdict::Ok);
        for label in [
            auth_label(Some("Bearer meta-session-token-of-a-login")),
            auth_label(None),
        ] {
            assert_eq!(key_verdict(&with(&label)), Verdict::Fail, "{label}");
        }
        let quiet: String = STUB_LOG
            .lines()
            .filter(|l| !l.contains(" kind=muse"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(key_verdict(&quiet), Verdict::Fail, "the control");
    }

    /// A file of the owner's Muse Code directories open in a Muse Code
    /// process is named, its config and its sessions alike.
    #[test]
    fn a_file_open_under_the_owners_muse_dirs_is_named() {
        let home = &["/Users//owner".to_string()];
        let lsof = "p4242\nfcwd\nn/private/tmp/at.lab0/proj\nf3\nn/Users//owner/.config/muse/auth.json\n\
                    f4\nn/Users//owner/.local/share/muse/sessions/a.jsonl\nf5\nn/Users//owner/.config/muse\n\
                    f6\nn/Users//owner/.config/muse-other/x\nf7\nn/s/runs/t/scratch/home/.config/muse/.auth.json.lock\n\
                    f8\nn/Users//owner/Documents/notes.txt\n\
                    f9\nn/Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db\n\
                    f10\nn/Users//owner/Library/Application Support/Museum/x\n";
        assert_eq!(
            held_under(lsof, home),
            [
                "/Users//owner/.config/muse/auth.json",
                "/Users//owner/.local/share/muse/sessions/a.jsonl",
                "/Users//owner/.config/muse",
                "/Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db",
            ]
        );
        let scratch_only = "p1\nn/s/runs/t/scratch/home/.config/muse/x\nn/Users//owner/.config/muse-other\n\
                            n/s/runs/t/scratch/home/Library/Application Support/Muse/x\n";
        assert!(held_under(scratch_only, home).is_empty(), "the control");
    }

    /// A kernel refusal naming the owner's Muse Code directories is named;
    /// one naming another directory of the owner's, or a sibling whose name
    /// only begins the same, is not.
    #[test]
    fn a_refusal_naming_the_owners_muse_dirs_is_named() {
        let home = &["/Users//owner".to_string()];
        let refusals = vec![
            "deny(1) file-read-data /Users//owner/.config/muse/auth.json".to_string(),
            "deny(1) file-read-metadata /Users//owner/.local/share/muse".to_string(),
            "deny(1) file-read-data /Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db"
                .to_string(),
            "deny(1) file-read-data /Users//owner/.config/muse-other/x".to_string(),
            "deny(1) file-read-data /Users//owner/Library/Keychains/login.keychain-db".to_string(),
            "deny(1) file-read-data /Users//owner/Library/Application Support/Museum".to_string(),
        ];
        assert_eq!(naming_own_dirs(&refusals, home), refusals[..3].to_vec());
        assert!(naming_own_dirs(&[], home).is_empty(), "the control");
        assert_eq!(refusals_verdict(&refusals[..1], true), Verdict::Fail);
        assert_eq!(refusals_verdict(&[], false), Verdict::Unproven);
    }

    /// UNDER A SCRATCH `HOME` THE ACCOUNT'S MUSE CODE DIRECTORIES ARE
    /// WATCHED: Muse Code 1.4.10 opened the account's own `session-names.db`
    /// whatever `HOME` said, so a file held, or a refusal named, under the
    /// account's home (the account database's) counts, as does one under the
    /// scratch home's own.
    #[test]
    fn under_a_scratch_home_the_account_muse_dirs_are_watched() {
        let homes = crate::paths::guarded("/Users//owner", "/private/tmp/s/home");
        let db =
            "/Users//owner/Library/Application Support/Muse/session-name-authority/session-names.db";
        let lsof = format!(
            "p7\nf3\nn{db}\nf4\nn/private/tmp/s/home/.config/muse/auth.json\nf5\nn/Users//owner/Documents/x\n"
        );
        assert_eq!(
            held_under(&lsof, &homes),
            [db, "/private/tmp/s/home/.config/muse/auth.json"]
        );
        let refusals = vec![format!("deny(1) file-read-data {db}")];
        assert_eq!(naming_own_dirs(&refusals, &homes), refusals);
        let env_only = crate::paths::guarded("", "/private/tmp/s/home");
        assert_eq!(
            held_under(&lsof, &env_only),
            ["/private/tmp/s/home/.config/muse/auth.json"]
        );
        assert!(naming_own_dirs(&refusals, &env_only).is_empty());
    }

    /// THE OWNER'S-DATA CHECK FAILS ON A HELD FILE whatever its control saw,
    /// holds when Muse Code wrote under its own scratch HOME and held
    /// nothing, and is UNPROVEN when it wrote nothing there or no Muse Code
    /// process was seen.
    #[test]
    fn the_owner_check_fails_on_a_held_file_and_needs_its_control() {
        let held = vec!["/Users//owner/.config/muse/auth.json".to_string()];
        let wrote = vec![".config/muse/.auth.json.lock".to_string()];
        assert_eq!(owner_verdict(&held, &wrote, &[7], true), Verdict::Fail);
        assert_eq!(owner_verdict(&held, &[], &[], false), Verdict::Fail);
        assert_eq!(owner_verdict(&[], &wrote, &[7], true), Verdict::Ok);
        assert_eq!(owner_verdict(&[], &[], &[7], true), Verdict::Unproven);
        assert_eq!(owner_verdict(&[], &wrote, &[], true), Verdict::Unproven);
        assert_eq!(owner_verdict(&[], &wrote, &[7], false), Verdict::Unproven);
        assert_eq!(owner_verdict(&held, &wrote, &[7], false), Verdict::Fail);
    }

    /// Only an admitted successful report proves an absence: a timeout,
    /// nonzero exit, warning or capture ceiling is unknown, never a clean
    /// observer.
    #[test]
    fn an_open_file_observer_must_return_a_complete_successful_report() {
        use crate::selftest::complete_open_files;
        use std::os::unix::process::ExitStatusExt as _;
        let output = |code, stdout: Vec<u8>, stderr: Vec<u8>| std::process::Output {
            status: std::process::ExitStatus::from_raw(code),
            stdout,
            stderr,
        };
        let wrote = vec![".config/muse/session".to_string()];
        let judge = |result: Result<std::process::Output, aterm_uds::deadline::RunError>| {
            let seen = complete_open_files(
                7,
                result.map_err(aterm_uds::deadline::CompleteOutputError::Run),
            );
            owner_verdict(
                &held_under(&seen.text, &["/Users//owner".to_string()]),
                &wrote,
                &[7],
                seen.complete,
            )
        };
        let clean = b"p7\nn/private/tmp/owned/home/.config/muse/session\n".to_vec();
        assert_eq!(judge(Ok(output(0, clean.clone(), Vec::new()))), Verdict::Ok);
        let held = b"p7\nn/Users//owner/.config/muse/auth.json\n".to_vec();
        assert_eq!(
            judge(Ok(output(0, held.clone(), Vec::new()))),
            Verdict::Fail
        );
        assert_eq!(
            judge(Ok(output(1 << 8, held, b"lsof: WARNING".to_vec()))),
            Verdict::Fail,
            "a failed report's evidence"
        );
        for result in [
            Err(aterm_uds::deadline::RunError::TimedOut),
            Err(aterm_uds::deadline::RunError::Spawn(std::io::Error::other(
                "injected failure",
            ))),
            Ok(output(1 << 8, clean.clone(), Vec::new())),
            Ok(output(0, clean, b"incomplete observer".to_vec())),
            Ok(output(0, Vec::new(), Vec::new())),
            Ok(output(0, b"p8\nn/elsewhere\n".to_vec(), Vec::new())),
            Ok(output(0, b"p7\nn/no-terminator".to_vec(), Vec::new())),
            Ok(output(0, vec![0xff], Vec::new())),
            Ok(output(
                0,
                vec![b'x'; aterm_uds::deadline::OUTPUT_KEEP],
                Vec::new(),
            )),
        ] {
            assert_eq!(judge(result), Verdict::Unproven);
        }
    }

    /// THE RUNTIME CHECK FAILS ON ANYTHING LEFT of what the tab's Muse Code
    /// made in the shared runtime directory, holds when its lease, socket and
    /// session record were all seen and none is left, and is UNPROVEN when the
    /// witness saw fewer (a Muse Code that made none, or a registry of another
    /// shape).
    #[test]
    fn the_runtime_check_fails_on_a_leftover_and_needs_all_three_seen() {
        let r = "/private/tmp/tbh-501-rt/muse";
        let made: Vec<String> = ["ms-aa.sock", "ms-aa.sock.lease", "sessions/01a1.json"]
            .iter()
            .map(|n| format!("{r}/{n}"))
            .collect();
        assert_eq!(runtime_verdict(&made, &[], true), Verdict::Ok);
        assert_eq!(runtime_verdict(&made, &made[1..2], true), Verdict::Fail);
        assert_eq!(runtime_verdict(&[], &made[..1], true), Verdict::Fail);
        assert_eq!(runtime_verdict(&made[1..], &[], true), Verdict::Unproven);
        assert_eq!(runtime_verdict(&made[..2], &[], true), Verdict::Unproven);
        assert_eq!(
            runtime_verdict(&[], &[], true),
            Verdict::Unproven,
            "the control"
        );
    }
}
