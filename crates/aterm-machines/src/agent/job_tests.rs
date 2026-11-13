// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran, Stat};

fn layout() -> Layout {
    Layout { root: "/A".into() }
}

fn body(action: Action) -> Envelope {
    Envelope {
        schema: 1,
        host: "U".into(),
        host_name: "lab-07".into(),
        serial: 5,
        issued: 1,
        expires: 2,
        action,
        disrupt: Disrupt::WhenIdle,
        idle_secs: 900,
        baseline: None,
        artifacts: Vec::new(),
        exec: None,
        upgrade: None,
        rekey: None,
    }
}

#[test]
fn disruption_waits_for_an_idle_or_empty_console_unless_told_now() {
    let mut e = body(Action::Apply);
    let mut facts = HostFacts {
        console_user: Some("alice".into()),
        idle_secs: Some(60),
        ..HostFacts::default()
    };
    assert!(!may_disrupt(&e, &facts));
    facts.idle_secs = Some(901);
    assert!(may_disrupt(&e, &facts));
    facts.idle_secs = None;
    assert!(!may_disrupt(&e, &facts), "an unknown idle time is not idle");
    facts.console_user = None;
    assert!(may_disrupt(&e, &facts));
    facts.console_user = Some("alice".into());
    e.disrupt = Disrupt::Now;
    assert!(may_disrupt(&e, &facts));
}

#[test]
fn the_journal_records_each_change_as_one_line_before_it_is_made() {
    let fake = Fake::root();
    let mut j = FileJournal::new(&fake, &layout(), "1-5");
    j.before_change("power.ac.sleep", "10", "0", None).unwrap();
    j.before_change("names.computer", "\"a\"", "\"b\"", None)
        .unwrap();
    let text = String::from_utf8(fake.written("/A/state/journal.ndjson").unwrap()).unwrap();
    let lines: Vec<aterm_json::Value> = text
        .lines()
        .map(|l| aterm_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"].as_str(), Some("power.ac.sleep"));
    assert_eq!(lines[1]["from"].as_str(), Some("\"a\""));
    assert_eq!(lines[1]["run"].as_str(), Some("1-5"));
    assert_eq!(fake.mode_of("/A/state/journal.ndjson"), Some(0o600));
}

/// An item that ended failed in an apply run is a line of its own, with its run and why.
/// NEGATIVE CONTROL: a journal that keeps changes alone (the trait's default) records none.
#[test]
fn the_journal_records_an_item_that_failed() {
    let fake = Fake::root();
    let mut j = FileJournal::new(&fake, &layout(), "1-5");
    j.before_change("power.ac.sleep", "10", "0", None).unwrap();
    j.failed("power.ac.sleep", "pmset exited 1: not permitted")
        .unwrap();
    let text = String::from_utf8(fake.written("/A/state/journal.ndjson").unwrap()).unwrap();
    let lines: Vec<aterm_json::Value> = text
        .lines()
        .map(|l| aterm_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[1]["id"].as_str(), Some("power.ac.sleep"));
    assert_eq!(
        lines[1]["failed"].as_str(),
        Some("pmset exited 1: not permitted")
    );
    assert_eq!(lines[1]["run"].as_str(), Some("1-5"));
}

fn root_dir() -> Stat {
    Stat {
        uid: 0,
        gid: 0,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    }
}

#[test]
fn the_job_refuses_a_run_directory_someone_else_could_have_written() {
    let fake = Fake::root();
    fake.stat_of(
        "/A/state/runs/1-5",
        Stat {
            uid: 501,
            ..root_dir()
        },
    );
    let err = accept(&fake, &layout(), "1-5").unwrap_err();
    assert!(err.contains("not the agent's own"), "{err}");
    let fake = Fake::root();
    fake.stat_of("/A/state/runs/1-5", root_dir());
    fake.file("/A/state/runs/1-5/envelope", b"x");
    let err = accept(&fake, &layout(), "1-5").unwrap_err();
    assert!(err.contains("readable by others"), "{err}");
}

#[test]
fn the_job_refuses_a_run_the_lock_does_not_name() {
    let fake = Fake::root();
    fake.stat_of("/A/state/runs/1-5", root_dir());
    fake.stat_of(
        "/A/state/runs/1-5/envelope",
        Stat {
            mode: 0o600,
            dir: false,
            ..root_dir()
        },
    );
    fake.file("/A/state/run.lock", b"pid=9 run=2-6\n");
    let err = accept(&fake, &layout(), "1-5").unwrap_err();
    assert!(err.contains("does not name run 1-5"), "{err}");
}

fn exec_of(argv: &[&str]) -> Envelope {
    let mut e = body(Action::Exec);
    e.exec = Some(envelope::Exec {
        argv: argv.iter().map(|&a| a.to_owned()).collect(),
        user: None,
        budget_secs: 30,
    });
    e
}

#[test]
fn exec_runs_one_absolute_argv_and_keeps_its_output_status_and_errors_apart() {
    let fake = Fake::root();
    fake.on("/usr/bin/sw_vers -buildVersion", Ran::ok("26A428\n"));
    let r = exec(&fake, &exec_of(&["/usr/bin/sw_vers", "-buildVersion"])).unwrap();
    assert_eq!(r.outcome, Outcome::Changed);
    assert_eq!(r.detail.as_deref(), Some("26A428"));
    assert_eq!(r.to.as_deref(), Some("exit 0"));
    assert_eq!(r.why, None);
    assert!(exec(&fake, &exec_of(&["sw_vers"])).is_err());
    fake.on(
        "/usr/bin/false",
        Ran {
            status: Some(1),
            stdout: b"half\n".to_vec(),
            stderr: b"no\nreally\n".to_vec(),
        },
    );
    let r = exec(&fake, &exec_of(&["/usr/bin/false"])).unwrap();
    assert_eq!(r.outcome, Outcome::Failed);
    assert_eq!(r.detail.as_deref(), Some("half"));
    assert_eq!(r.why.as_deref(), Some("no\nreally"));
    assert_eq!(r.to.as_deref(), Some("exit 1"));
    let r = exec(&fake, &exec_of(&["/usr/bin/missing"])).unwrap();
    assert_eq!(r.outcome, Outcome::Failed);
    assert_eq!(r.to, None);
    assert!(r.why.is_some());
}

#[test]
fn an_exec_row_reads_back_as_the_run_it_reports() {
    let fake = Fake::root();
    let denied = "Error: unable to open database \"/Library/Application Support/com.apple.TCC/TCC.db\": authorization denied";
    for (argv, ran) in [
        (["/usr/bin/sw_vers", "-buildVersion"], Ran::ok("26A428")),
        (
            ["/usr/bin/sqlite3", "TCC.db"],
            Ran {
                status: Some(1),
                stdout: b"partial".to_vec(),
                stderr: denied.as_bytes().to_vec(),
            },
        ),
        (["/usr/bin/git", "config"], Ran::exit(1, "")),
        (
            ["/bin/sleep", "60"],
            Ran {
                status: None,
                stdout: b"so far".to_vec(),
                stderr: Vec::new(),
            },
        ),
    ] {
        let display = argv.join(" ");
        fake.on(&display, ran.clone());
        let row = exec(&fake, &exec_of(&argv)).unwrap();
        assert_eq!(exec_ran(&row), Ok(ran), "{display}: {row:?}");
    }
    assert_eq!(
        exec(&fake, &exec_of(&["/bin/sleep", "60"]))
            .unwrap()
            .to
            .as_deref(),
        Some("killed")
    );
    let row = exec(&fake, &exec_of(&["/usr/bin/missing"])).unwrap();
    assert_eq!(exec_ran(&row), Err(row.why.clone().unwrap()));
    let mut other = row;
    other.family = "power".into();
    assert!(exec_ran(&other).is_err());
}

#[test]
fn rekey_refuses_an_allowed_signers_that_would_lock_the_fleet_out() {
    let fake = Fake::root();
    let mut e = body(Action::Rekey);
    e.rekey = Some("# nobody\n".into());
    assert!(rekey(&fake, &layout(), &e).is_err());
    assert!(fake.written("/A/allowed_signers").is_none());
    let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";
    e.rekey = Some(format!(
        "new@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
    ));
    let r = rekey(&fake, &layout(), &e).unwrap();
    assert_eq!(r.to.as_deref(), Some("new@ctl"));
    assert_eq!(fake.mode_of("/A/allowed_signers"), Some(0o644));
}

#[test]
fn a_key_rekeyed_under_a_new_principal_keeps_its_floor() {
    let fake = Fake::root();
    fake.file(
        "/A/state/floors.toml",
        b"[[floor]]\nprincipal = \"signing@ctl\"\nserial = 7\n",
    );
    let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";
    let mut e = body(Action::Rekey);
    e.rekey = Some(format!(
        "daily@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
    ));
    rekey(&fake, &layout(), &e).unwrap();
    let floors = state::floors_or_refuse(&fake, &layout()).unwrap();
    assert_eq!(state::floor_of(&floors, "daily@ctl"), 7);
    let calls = fake.calls();
    let raised = calls
        .iter()
        .position(|c| c == "write /A/state/floors.toml 644")
        .unwrap();
    let listed = calls
        .iter()
        .position(|c| c == "write /A/allowed_signers 644")
        .unwrap();
    assert!(
        raised < listed,
        "the floor rises before the new list can verify"
    );
}

#[test]
fn a_bundle_is_found_by_its_executable_and_a_dev_archive_by_its_one_binary() {
    let fake = Fake::root();
    fake.file("/P/aterm.app/Contents/MacOS/aterm", b"x");
    assert_eq!(
        find_executable(&fake, Path::new("/P")).unwrap(),
        (PathBuf::from("aterm.app/Contents/MacOS/aterm"), true)
    );
    let fake = Fake::root();
    fake.file("/P/aterm-machines-dev", b"x");
    assert_eq!(
        find_executable(&fake, Path::new("/P")).unwrap(),
        (PathBuf::from("aterm-machines-dev"), false)
    );
    let fake = Fake::root();
    fake.file("/P/a", b"x").file("/P/b", b"y");
    assert!(find_executable(&fake, Path::new("/P")).is_err());
}

/// Seed `dir` as a directory the fake lists in its parent.
fn listed_dir(fake: &Fake, dir: &str) {
    fake.file(dir, b"").stat_of(dir, root_dir());
}

#[test]
fn a_bundle_of_any_name_is_found_when_it_is_the_only_one_and_holds_aterm() {
    let fake = Fake::root();
    listed_dir(&fake, "/P/aterm (dev).app");
    fake.file("/P/aterm (dev).app/Contents/MacOS/aterm", b"x");
    assert_eq!(
        find_executable(&fake, Path::new("/P")).unwrap(),
        (PathBuf::from("aterm (dev).app/Contents/MacOS/aterm"), true)
    );
    listed_dir(&fake, "/P/Other.app");
    fake.file("/P/Other.app/Contents/MacOS/aterm", b"y");
    assert!(find_executable(&fake, Path::new("/P")).is_err());
    let fake = Fake::root();
    listed_dir(&fake, "/P/Other.app");
    fake.file("/P/Other.app/Contents/MacOS/other", b"x");
    assert!(find_executable(&fake, Path::new("/P")).is_err());
}

/// The version the upgrade names and the staged helper reports: equal, as for a sound
/// payload.
const HELPER_VERSION: &str = "0.156.0";

/// A host mid-upgrade.
fn upgrading_at(rel: &str, exe_uid: u32) -> (Fake, Envelope) {
    let fake = Fake::root();
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(b"the archive");
    let sha = aterm_codec::hex::encode_lower(&hasher.finalize());
    fake.file(&format!("/A/cache/{sha}"), b"the archive");
    for d in [
        "/",
        "/A",
        "/A/stage",
        "/A/stage/upgrade-1-5",
        "/A/stage/upgrade-1-5/payload",
        "/A/agent",
        "/A/bin",
    ] {
        fake.stat_of(d, root_dir());
    }
    let payload = "/A/stage/upgrade-1-5/payload";
    let mut dir = PathBuf::from(payload);
    let parents: Vec<&str> = rel.split('/').collect();
    for (i, part) in parents[..parents.len() - 1].iter().enumerate() {
        dir.push(part);
        let d = dir.to_string_lossy().into_owned();
        if i == 0 {
            listed_dir(&fake, &d);
        } else {
            fake.stat_of(&d, root_dir());
        }
    }
    let exe = format!("{payload}/{rel}");
    fake.file(&exe, b"new agent");
    fake.stat_of(
        &exe,
        Stat {
            uid: exe_uid,
            dir: false,
            ..root_dir()
        },
    );
    fake.file(
        "/A/managed.toml",
        b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"0.1.0\"\nexecutable = \"agent/aterm-machines-dev\"\ninstalled = 0\n",
    );
    fake.on(
        &format!("/usr/bin/ditto -x -k /A/cache/{sha} {payload}"),
        Ran::ok(""),
    );
    for c in [
        "/usr/sbin/chown -R root:wheel /A/stage/upgrade-1-5/payload",
        "/bin/chmod -R go-w /A/stage/upgrade-1-5/payload",
        "/bin/chmod 0755 /A/stage/upgrade-1-5/payload",
    ] {
        fake.on(c, Ran::ok(""));
    }
    fake.on(
        &format!("{} machines agent version", crate::port::shell_word(&exe)),
        Ran::ok(&format!("{HELPER_VERSION}\n")),
    );
    fake.on(
        "/usr/sbin/visudo -cf /A/stage/sudoers.check",
        Ran::ok("/A/stage/sudoers.check: parsed OK\n"),
    );
    let mut e = body(Action::Upgrade);
    e.upgrade = Some(envelope::Upgrade {
        sha256: sha,
        kind: "dev-zip".into(),
        version: HELPER_VERSION.into(),
    });
    (fake, e)
}

/// [`upgrading_at`] a dev binary.
fn upgrading(exe_uid: u32) -> (Fake, Envelope) {
    upgrading_at("aterm-machines-dev", exe_uid)
}

fn cached_archive(e: &Envelope) -> String {
    format!("/A/cache/{}", e.upgrade.as_ref().unwrap().sha256)
}

#[test]
fn an_upgrade_is_made_roots_and_checked_before_it_runs_and_before_the_swap() {
    let (fake, e) = upgrading(0);
    let r = upgrade(&fake, &layout(), &e, "1-5").unwrap();
    assert_eq!(r.outcome, Outcome::Changed);
    let revert = r.revert.unwrap();
    assert!(revert.contains("/A/agent.prev"), "{revert}");
    let calls = fake.calls();
    let at = |c: &str| {
        calls
            .iter()
            .position(|x| x == c)
            .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
    };
    let own = at("/usr/sbin/chown -R root:wheel /A/stage/upgrade-1-5/payload");
    let probe = at("/A/stage/upgrade-1-5/payload/aterm-machines-dev machines agent version");
    let check = at("/usr/sbin/visudo -cf /A/stage/sudoers.check");
    let keep = at("rename /A/agent /A/agent.prev");
    let swap = at("rename /A/stage/upgrade-1-5/payload /A/agent");
    let rule = at("write /etc/sudoers.d/aterm-machines 440");
    assert!(own < probe && probe < check && check < keep && keep < swap && swap < rule);
    let sudoers =
        String::from_utf8(fake.written("/etc/sudoers.d/aterm-machines").unwrap()).unwrap();
    let digest = fake
        .sha256(Path::new("/A/agent/aterm-machines-dev"))
        .unwrap();
    assert!(sudoers.contains(&format!("sha256:{digest} ")), "{sudoers}");
    assert!(rule < at(&format!("remove {}", cached_archive(&e))));
    assert!(fake.written(&cached_archive(&e)).is_none());
}

const VERSION: &str = "/A/stage/upgrade-1-5/payload/aterm-machines-dev machines agent version";

#[test]
fn the_new_agent_runs_within_the_first_exec_hang_and_its_answer_proves_its_folder() {
    let (fake, e) = upgrading(0);
    let budget = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::clone(&budget);
    fake.simulate(move |_, cmd| {
        if cmd.display() == VERSION {
            *seen.lock().unwrap() = Some(cmd.budget);
        }
        None
    });
    fake.awake_at(5_000_000);
    upgrade(&fake, &layout(), &e, "1-5").unwrap();
    assert_eq!(
        *budget.lock().unwrap(),
        Some(aterm_uds::deadline::FIRST_EXEC_HANG)
    );
    let calls = fake.calls();
    let at = |c: &str| {
        calls
            .iter()
            .position(|x| x == c)
            .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
    };
    let stamped = at("write /A/stage/upgrade-1-5/exec 644");
    let ran = at("write /A/stage/upgrade-1-5/ran 644");
    assert!(stamped < at(VERSION) && at(VERSION) < ran, "{calls:#?}");
    assert!(ran < at("rename /A/stage/upgrade-1-5/payload /A/agent"));
}

#[test]
fn an_upgrade_first_sweeps_each_earlier_upgrade_folder_that_holds_nothing_back() {
    let (fake, e) = upgrading(0);
    fake.awake_at(10_000_000);
    for (name, stamp, ran) in [
        ("upgrade-1-1", None, false),
        ("upgrade-1-2", Some("1000000"), false),
        ("upgrade-1-3", Some("9900000"), true),
        ("upgrade-1-4", Some("9900000"), false),
    ] {
        let dir = format!("/A/stage/{name}");
        listed_dir(&fake, &dir);
        fake.file(&format!("{dir}/aterm"), b"agent");
        if let Some(stamp) = stamp {
            fake.file(&format!("{dir}/exec"), stamp.as_bytes());
        }
        if ran {
            fake.file(&format!("{dir}/ran"), b"");
        }
    }
    fake.file("/A/stage/upgrade-link", b"").stat_of(
        "/A/stage/upgrade-link",
        Stat {
            dir: false,
            symlink: true,
            ..root_dir()
        },
    );
    fake.file("/A/stage/aterm-open-1.sig", b"sig");
    upgrade(&fake, &layout(), &e, "1-5").unwrap();
    for (gone, why) in [
        ("upgrade-1-1", "nothing was started from it"),
        ("upgrade-1-2", "its hold is spent"),
        ("upgrade-1-3", "its program showed it ran"),
    ] {
        assert!(
            fake.written(&format!("/A/stage/{gone}/aterm")).is_none(),
            "{gone}: {why}"
        );
    }
    assert!(
        fake.written("/A/stage/upgrade-1-4/aterm").is_some(),
        "a program started from it 100 seconds ago has not shown it ran"
    );
    assert!(fake.written("/A/stage/aterm-open-1.sig").is_some());
    let calls = fake.calls();
    assert!(!calls.contains(&"remove /A/stage/upgrade-link".to_owned()));
    let at = |c: &str| {
        calls
            .iter()
            .position(|x| x == c)
            .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
    };
    assert!(at("remove /A/stage/upgrade-1-2") < at("mkdir /A/stage/upgrade-1-5 700"));
}

#[test]
fn a_new_agent_a_signal_ended_leaves_its_folder_held_in_place() {
    let (fake, e) = upgrading(0);
    fake.simulate(|_, cmd| {
        (cmd.display() == VERSION).then(|| Ran {
            status: None,
            stdout: Vec::new(),
            stderr: b"Killed: 9".to_vec(),
        })
    });
    fake.awake_at(5_000_000);
    let err = upgrade(&fake, &layout(), &e, "1-5").unwrap_err();
    assert!(err.contains("killed by a signal"), "{err}");
    let calls = fake.calls();
    let run = calls.iter().position(|c| c == VERSION).unwrap();
    assert!(
        !calls[run..]
            .iter()
            .any(|c| c.starts_with("rename") || c.starts_with("remove")),
        "{calls:#?}"
    );
    assert!(
        fake.written("/A/stage/upgrade-1-5/payload/aterm-machines-dev")
            .is_some()
    );
    assert_eq!(
        crate::first_exec::hold_left(&fake, Path::new("/A/stage/upgrade-1-5")),
        Some(aterm_uds::deadline::FIRST_EXEC_HANG)
    );
}

#[test]
fn a_dev_bundle_upgrades_on_its_signed_hash_and_bin_aterm_points_into_it() {
    let rel = "aterm (dev).app/Contents/MacOS/aterm";
    let (fake, e) = upgrading_at(rel, 0);
    upgrade(&fake, &layout(), &e, "1-5").unwrap();
    let calls = fake.calls();
    assert!(
        calls.contains(&format!("symlink /A/bin/.aterm.new -> /A/agent/{rel}")),
        "{calls:#?}"
    );
    assert!(!calls.iter().any(|c| c.contains("codesign")), "{calls:#?}");
    assert_eq!(
        state::managed(&fake, &layout()).unwrap().executable,
        format!("agent/{rel}")
    );
}

#[test]
fn an_app_zip_is_checked_against_the_team_on_the_bundle_it_holds() {
    let (fake, mut e) = upgrading_at("aterm (dev).app/Contents/MacOS/aterm", 0);
    e.upgrade.as_mut().unwrap().kind = "app-zip".into();
    let codesign = format!(
        "/usr/bin/codesign --verify --deep --strict -R {} {}",
        crate::port::shell_word(&format!(
            "=anchor apple generic and certificate leaf[subject.OU] = \"{ATERM_TEAM}\""
        )),
        crate::port::shell_word("/A/stage/upgrade-1-5/payload/aterm (dev).app"),
    );
    fake.on(
        &codesign,
        Ran::exit(
            3,
            "test-requirement: code failed to satisfy specified code requirement(s)",
        ),
    );
    let err = upgrade(&fake, &layout(), &e, "1-5").unwrap_err();
    assert!(err.contains("aterm (dev).app is not signed by"), "{err}");
    let calls = fake.calls();
    assert!(calls.contains(&codesign), "{calls:#?}");
    assert!(!calls.iter().any(|c| c.starts_with("rename")), "{calls:#?}");
    assert!(fake.written(&cached_archive(&e)).is_some());
}

#[test]
fn an_upgrade_someone_else_owns_is_refused_and_the_old_agent_stays() {
    let (fake, e) = upgrading(501);
    let err = upgrade(&fake, &layout(), &e, "1-5").unwrap_err();
    assert!(err.contains("owned by uid 501"), "{err}");
    let calls = fake.calls();
    assert!(
        !calls.iter().any(|c| c.contains("machines agent version")),
        "{calls:#?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("rename")), "{calls:#?}");
    assert!(fake.written("/etc/sudoers.d/aterm-machines").is_none());
    assert!(fake.written(&cached_archive(&e)).is_some());
}

#[test]
fn progress_keeps_the_started_line_apply_wrote() {
    let fake = Fake::root();
    fake.file(
        "/A/state/runs/1-5/progress.ndjson",
        b"{\"started\":{\"run\":\"1-5\"}}\n",
    );
    let mut p = Progress::resume(&fake, Path::new("/A/state/runs/1-5"));
    p.push(&Line::Refused { why: "x".into() });
    let text =
        String::from_utf8(fake.written("/A/state/runs/1-5/progress.ndjson").unwrap()).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.starts_with("{\"started\""));
}

#[test]
fn exec_ran_rejects_plus_prefixed_exit_status() {
    let mut r = row("exec.check", "exec", "check".into());
    r.to = Some("exit +0".into());
    assert!(exec_ran(&r).is_err());
    r.to = Some("exit 0".into());
    assert_eq!(exec_ran(&r).unwrap().status, Some(0));
}
