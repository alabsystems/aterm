// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::HostFacts;
use crate::port::Fake;

/// `launchctl print-disabled system`, captured in the macOS 27.0 VM (26A428) with Remote
/// Login and Screen Sharing turned on in System Settings.
const PRINT_DISABLED_VM: &str = "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.apple.CSCSupportd\" => disabled\n\t\t\"com.apple.mdmclient.daemon.runatboot\" => disabled\n\t\t\"com.apple.ftpd\" => disabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n";

/// The same on a Mac (27.0.1) where Screen Sharing was never turned on: no override.
const PRINT_DISABLED_NO_SCREEN_SHARING: &str = "\n\tdisabled services = {\n\t\t\"com.apple.CSCSupportd\" => disabled\n\t\t\"com.apple.ftpd\" => disabled\n\t\t\"com.apple.mdmclient.daemon.runatboot\" => disabled\n\t\t\"com.openssh.sshd\" => enabled\n\t\t\"com.apple.dhcp6d\" => enabled\n\t}\n";

/// The VM's rows for the screen-sharing agent and aterm, read as root over SSH with remote
/// FDA on: Screen Sharing granted, aterm listed under Full Disk Access but switched off.
const TCC_VM: &str = "com.apple.screensharing.agent|kTCCServicePostEvent|2\ncom.apple.screensharing.agent|kTCCServiceScreenCapture|2\ncom.aterm.aterm|kTCCServiceSystemPolicyAllFiles|0\n";

fn ctx() -> Ctx {
    Ctx {
        facts: HostFacts::default(),
        users: Vec::new(),
        console: None,
        host_name: "lab-07".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    }
}

fn admin() -> User {
    User {
        name: "admin".into(),
        uid: 501,
        gid: 20,
        home: "/Users//admin".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    }
}

fn item(id: &str) -> PersonItem {
    PersonItem {
        step: PersonStep::parse(id).unwrap(),
    }
}

fn rows_of(text: &str) -> Result<Vec<TccRow>, String> {
    parse_tcc(text)
}

fn denied() -> Result<Vec<TccRow>, String> {
    Err(NEEDS_REMOTE_FDA.to_owned())
}

#[test]
fn the_remote_tcc_cmd_routes_through_the_agents_native_tcc_verb() {
    assert_eq!(
        tcc_cmd(&["com.aterm.aterm"]).display(),
        "/Library/Management/aterm/bin/aterm machines agent tcc com.aterm.aterm"
    );
    assert_eq!(
        tcc_open_cmd().display(),
        "/Library/Management/aterm/bin/aterm machines agent tcc"
    );

    let sys = Fake::root();
    sys.on_tcc(
        &["com.aterm.aterm"],
        rows_of("com.aterm.aterm|kTCCServiceDeveloperTool|2\n"),
    );
    let mut out = Vec::new();
    assert_eq!(
        agent_tcc(&sys, &["com.aterm.aterm"], &mut out),
        std::process::ExitCode::SUCCESS
    );
    let step = PersonStep::parse("developer_tools:aterm").unwrap();
    let probe = probe_step(&step, Known::default(), &mut |cmd| {
        assert_eq!(cmd, &tcc_cmd(&["com.aterm.aterm"]));
        Ok(Ran::ok(&String::from_utf8(out.clone()).unwrap()))
    });
    assert_eq!(
        probe,
        Probe::Ok {
            now: "granted".into()
        }
    );

    let refused_sys = Fake::root();
    refused_sys.on_tcc(&[], denied());
    let mut refused_out = Vec::new();
    assert_eq!(
        agent_tcc(&refused_sys, &[], &mut refused_out),
        std::process::ExitCode::FAILURE
    );
    let remote_fda = PersonStep::parse("remote_full_disk_access").unwrap();
    let fda_probe = probe_step(&remote_fda, Known::default(), &mut |cmd| {
        assert_eq!(cmd, &tcc_open_cmd());
        Ok(Ran {
            status: Some(1),
            stdout: refused_out.clone(),
            stderr: Vec::new(),
        })
    });
    assert!(matches!(fda_probe, Probe::Person { .. }), "{fda_probe:?}");

    let mut bad_out = Vec::new();
    assert_eq!(
        agent_tcc(&Fake::root(), &["com.x');drop"], &mut bad_out),
        std::process::ExitCode::from(crate::agent::REFUSED)
    );
    assert!(String::from_utf8_lossy(&bad_out).starts_with("refused: "));
}

/// THE READ TAKES NO LOCK ON tccd's FILE AND SPAWNS NO SUBPROCESS: [`read_tcc_file`]
/// answers rows from a WAL-backed database while the `EXCLUSIVE` byte-range lock a
/// committing writer holds (`PENDING_BYTE` and the 511 bytes after it) is taken.
#[cfg(target_os = "macos")]
#[test]
fn the_tcc_read_takes_no_lock_a_writer_would_meet() {
    use std::os::fd::AsRawFd as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let db = std::fs::canonicalize(dir.path())
        .expect("canonical")
        .join("TCC.db");
    let made = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
        .arg(&db)
        .arg(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE access (service TEXT, client TEXT, client_type INTEGER, auth_value INTEGER); \
             INSERT INTO access VALUES ('kTCCServiceDeveloperTool', 'com.aterm.aterm', 0, 2); \
             INSERT INTO access VALUES ('kTCCServicePostEvent', 'com.apple.screensharing.agent', 0, 2);",
        )
        .output()
        .expect("made");
    assert!(made.status.success(), "{made:?}");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&db)
        .expect("open to lock");
    // SAFETY: a zeroed `flock` is a valid value of the plain C struct.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = 0x4000_0000;
    fl.l_len = 512;
    // SAFETY: `fl` is valid for the call and the descriptor is open.
    let rc = unsafe { libc::fcntl(held.as_raw_fd(), libc::F_SETLK, &raw const fl) };
    assert_eq!(rc, 0, "the writer's lock is taken");
    let control = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
        .args(["-readonly", "-bail"])
        .arg(&db)
        .arg("SELECT client, service, auth_value FROM access WHERE client = 'com.aterm.aterm'")
        .output()
        .expect("sqlite3 control");
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("database is locked"),
        "{control:?}"
    );
    assert_eq!(read_tcc_file(&db, &[]), Ok(Vec::new()));
    assert_eq!(
        read_tcc_file(&db, &["com.aterm.aterm"]),
        Ok(vec![TccRow {
            client: "com.aterm.aterm".into(),
            service: "kTCCServiceDeveloperTool".into(),
            auth_value: 2,
        }])
    );
    drop(held);

    let corrupt = dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"not a sqlite file").expect("corrupt");
    let bad = read_tcc_file(&corrupt, &["com.aterm.aterm"]).unwrap_err();
    assert_ne!(bad, NEEDS_REMOTE_FDA);
}

#[test]
fn a_database_that_fails_to_open_for_another_reason_is_unusable_not_the_fda_switch() {
    let gone = Err("the TCC database did not answer: unable to open database file".to_owned());
    let sys = Fake::root();
    sys.on_tcc(&[], gone.clone());
    assert!(matches!(
        item("remote_full_disk_access").probe(&sys, &ctx()),
        Probe::Unusable { .. }
    ));
    let sys = Fake::root();
    sys.on_tcc(&["com.aterm.aterm"], gone);
    let probe = item("developer_tools:aterm").probe(&sys, &ctx());
    assert!(
        matches!(&probe, Probe::Unusable { why } if why != NEEDS_REMOTE_FDA),
        "{probe:?}"
    );
}

#[test]
fn rows_parse_from_the_vms_output() {
    let rows = parse_tcc(TCC_VM).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        grant(&rows, "com.aterm.aterm", "kTCCServiceSystemPolicyAllFiles"),
        Read::Value(0)
    );
    assert_eq!(
        grant(&rows, "com.aterm.aterm", "kTCCServiceDeveloperTool"),
        Read::Absent
    );
    assert!(parse_tcc("not a row\n").is_err());
}

#[test]
fn launchd_overrides_read_enabled_disabled_and_absent_apart() {
    assert_eq!(
        launchd_enabled(PRINT_DISABLED_VM, SSHD_LABEL),
        Read::Value(true)
    );
    assert_eq!(
        launchd_enabled(PRINT_DISABLED_VM, "com.apple.ftpd"),
        Read::Value(false)
    );
    assert_eq!(
        launchd_enabled(PRINT_DISABLED_NO_SCREEN_SHARING, SCREEN_SHARING_LABEL),
        Read::Absent
    );
    assert!(matches!(
        launchd_enabled("Bad request.\n", SSHD_LABEL),
        Read::Unusable(_)
    ));
}

#[test]
fn remote_login_reads_ok_from_the_vm_and_is_a_person_step_when_off() {
    let sys = Fake::root();
    sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
    assert_eq!(
        item("remote_login").probe(&sys, &ctx()),
        Probe::Ok { now: "on".into() }
    );
    let sys = Fake::root();
    sys.on(
        &print_disabled_cmd().display(),
        Ran::ok(&PRINT_DISABLED_VM.replace("\"com.openssh.sshd\" => enabled", "")),
    );
    let Probe::Person { step, how } = item("remote_login").probe(&sys, &ctx()) else {
        panic!("expected a person step");
    };
    assert_eq!(step, "remote_login");
    assert!(how.contains("Sharing \u{25b8} Remote Login"), "{how}");
    assert!(how.ends_with(PANE_REMOTE_LOGIN), "{how}");
}

#[test]
fn screen_sharing_needs_the_service_on_and_both_agent_grants() {
    let sys = Fake::root();
    sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
    sys.on_tcc(&[SCREEN_SHARING_AGENT], rows_of(TCC_VM));
    assert_eq!(
        item("screen_sharing").probe(&sys, &ctx()),
        Probe::Ok {
            now: "on, with control".into()
        }
    );
    let sys = Fake::root();
    sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
    sys.on_tcc(
        &[SCREEN_SHARING_AGENT],
        rows_of("com.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
    );
    assert!(matches!(
        item("screen_sharing").probe(&sys, &ctx()),
        Probe::Person { .. }
    ));
    let sys = Fake::root();
    sys.on(
        &print_disabled_cmd().display(),
        Ran::ok(PRINT_DISABLED_NO_SCREEN_SHARING),
    );
    assert!(matches!(
        item("screen_sharing").probe(&sys, &ctx()),
        Probe::Person { .. }
    ));
    assert_eq!(sys.calls().len(), 1);
}

#[test]
fn full_disk_access_is_granted_only_at_auth_value_two() {
    let sys = Fake::root();
    sys.on_tcc(&["com.aterm.aterm"], rows_of(TCC_VM));
    let Probe::Person { step, how } = item("full_disk_access:aterm").probe(&sys, &ctx()) else {
        panic!("aterm is listed but off in the VM");
    };
    assert_eq!(step, "full_disk_access:aterm");
    assert!(
        how.contains("Privacy & Security \u{25b8} Full Disk Access"),
        "{how}"
    );
    assert!(how.ends_with(PANE_FULL_DISK_ACCESS), "{how}");

    let sys = Fake::root();
    sys.on_tcc(
        &["com.aterm.aterm"],
        rows_of("com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n"),
    );
    assert_eq!(
        item("full_disk_access:aterm").probe(&sys, &ctx()),
        Probe::Ok {
            now: "granted".into()
        }
    );
}

#[test]
fn developer_tools_reads_its_own_service_and_an_unlisted_app_is_a_person_step() {
    let sys = Fake::root();
    sys.on_tcc(
        &["com.aterm.aterm"],
        rows_of("com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n"),
    );
    let Probe::Person { step, how } = item("developer_tools:aterm").probe(&sys, &ctx()) else {
        panic!("aterm is not in Developer Tools");
    };
    assert_eq!(step, "developer_tools:aterm");
    assert!(how.ends_with(PANE_DEVELOPER_TOOLS), "{how}");
    let sys = Fake::root();
    sys.on_tcc(
        &["com.aterm.aterm"],
        rows_of("com.aterm.aterm|kTCCServiceDeveloperTool|2\n"),
    );
    assert!(matches!(
        item("developer_tools:aterm").probe(&sys, &ctx()),
        Probe::Ok { .. }
    ));
}

#[test]
fn a_closed_tcc_database_is_unusable_never_a_grant_or_a_person_step() {
    for id in [
        "screen_sharing",
        "full_disk_access:aterm",
        "developer_tools:aterm",
    ] {
        let sys = Fake::root();
        sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
        sys.on_tcc(&[SCREEN_SHARING_AGENT], denied());
        sys.on_tcc(&["com.aterm.aterm"], denied());
        assert_eq!(
            item(id).probe(&sys, &ctx()),
            Probe::Unusable {
                why: NEEDS_REMOTE_FDA.into()
            },
            "{id}"
        );
    }
}

#[test]
fn remote_fda_comes_from_the_facts_or_from_opening_the_database() {
    let mut c = ctx();
    c.facts.remote_full_disk_access = Some(true);
    let sys = Fake::root();
    assert!(matches!(
        item("remote_full_disk_access").probe(&sys, &c),
        Probe::Ok { .. }
    ));
    assert!(sys.calls().is_empty(), "the fact answers without a read");
    let sys = Fake::root();
    sys.on_tcc(&[], denied());
    let Probe::Person { step, how } = item("remote_full_disk_access").probe(&sys, &ctx()) else {
        panic!("a refused open is the person step");
    };
    assert_eq!(step, "remote_full_disk_access");
    assert!(
        how.contains("Allow full disk access for remote users"),
        "{how}"
    );
    let sys = Fake::root();
    sys.on_tcc(&[], Ok(Vec::new()));
    assert!(matches!(
        item("remote_full_disk_access").probe(&sys, &ctx()),
        Probe::Ok { .. }
    ));
}

/// `aterm ctl privacy --json` in the VM (aterm 0.104.0, no FDA yet), cut to the fields
/// the fallback reads.
const PRIVACY_VM: &str = r#"{"schema":1,"platform":"macos","os":"27.0","identity":{"bundle_id":"com.aterm.aterm"},"full_disk_access":"unknown","probe":"open_errno","devtools":{"state":"unknown","subject":"com.aterm.aterm","grant":"unknown","list":"unknown","list_probe":"no_full_disk_access"}}"#;

#[test]
fn with_the_database_closed_aterms_own_record_answers_for_its_full_disk_access() {
    let mut c = ctx();
    c.console = Some(admin());
    let record = "admin$ /Applications/aterm.app/Contents/MacOS/aterm ctl privacy --json";
    let sys = Fake::root();
    sys.on_tcc(&["com.aterm.aterm"], denied());
    sys.on(record, Ran::ok(PRIVACY_VM));
    assert!(matches!(
        item("full_disk_access:aterm").probe(&sys, &c),
        Probe::Person { .. }
    ));
    let sys = Fake::root();
    sys.on_tcc(&["com.aterm.aterm"], denied());
    sys.on(
        record,
        Ran::ok(&PRIVACY_VM.replace("no_full_disk_access", "read")),
    );
    assert!(matches!(
        item("full_disk_access:aterm").probe(&sys, &c),
        Probe::Ok { .. }
    ));
    let sys = Fake::root();
    sys.on_tcc(&["com.aterm.aterm"], denied());
    sys.on(
        record,
        Ran::ok(
            &PRIVACY_VM
                .replace(
                    "\"bundle_id\":\"com.aterm.aterm\"",
                    "\"bundle_id\":\"com.aterm.aterm.dev\"",
                )
                .replace("no_full_disk_access", "read"),
        ),
    );
    assert_eq!(
        item("full_disk_access:aterm").probe(&sys, &c),
        Probe::Unusable {
            why: NEEDS_REMOTE_FDA.into()
        }
    );
    let sys = Fake::root();
    sys.on_tcc(&["com.aterm.aterm"], denied());
    assert_eq!(
        item("full_disk_access:aterm").probe(&sys, &c),
        Probe::Unusable {
            why: NEEDS_REMOTE_FDA.into()
        }
    );
}

#[test]
fn a_probe_that_cannot_run_its_reader_is_unusable() {
    let sys = Fake::root();
    let probe = item("full_disk_access:aterm").probe(&sys, &ctx());
    assert!(matches!(probe, Probe::Unusable { .. }), "{probe:?}");
    let probe = item("remote_login").probe(&sys, &ctx());
    assert!(matches!(probe, Probe::Unusable { .. }), "{probe:?}");
}

#[test]
fn apply_refuses_because_only_a_person_can_flip_the_switch() {
    let err = item("developer_tools:aterm")
        .apply(&Fake::root(), &ctx())
        .unwrap_err();
    assert!(err.starts_with("only a person can do this"), "{err}");
}

#[test]
fn step_ids_round_trip_and_bad_ones_are_refused() {
    for id in [
        "remote_login",
        "remote_full_disk_access",
        "screen_sharing",
        "full_disk_access:aterm",
        "developer_tools:com.example.Tool",
    ] {
        assert_eq!(PersonStep::parse(id).unwrap().id(), id);
    }
    assert!(PersonStep::parse("full_disk_access").is_err());
    assert!(PersonStep::parse("remote_login:aterm").is_err());
    assert!(PersonStep::parse("full_disk_access:emacs").is_err());
    assert!(PersonStep::parse("full_disk_access:com.x');drop").is_err());
    assert!(PersonStep::parse("firewall").is_err());
}

#[test]
fn the_table_expands_in_person_order_with_fda_before_developer_tools() {
    let table = Table {
        remote_login: Some(true),
        remote_full_disk_access: Some(true),
        screen_sharing: Some(false),
        full_disk_access: vec!["aterm".into()],
        developer_tools: vec!["aterm".into()],
    };
    let ids: Vec<String> = expand(&table, &ctx())
        .unwrap()
        .iter()
        .map(|i| i.id())
        .collect();
    assert_eq!(
        ids,
        [
            "person.remote_login",
            "person.remote_full_disk_access",
            "person.full_disk_access:aterm",
            "person.developer_tools:aterm",
        ]
    );
    let bad = Table {
        full_disk_access: vec!["Emacs".into()],
        ..Table::default()
    };
    assert!(expand(&bad, &ctx()).is_err());
    assert!(parse_tcc("ai.aterm|kTCCServiceSystemPolicyAllFiles|+2\n").is_err());
}
