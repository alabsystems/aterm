// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::Fake;
use aterm_uds::{AcceptWait, AcceptedMode};

const ROOT: &str = "/ctl/machines";
const UUID_1: &str = "0A1B2C3D-0000-4000-8000-000000000001";
const UUID_2: &str = "0A1B2C3D-0000-4000-8000-000000000002";
const SIGNING_BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
const RECOVERY_BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";

const HOST_KEY_CHANGED: &str = "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\nHost key for 0a1b2c3d-0000-4000-8000-000000000001 has changed and you have requested strict checking.\r\nHost key verification failed.\r\n";

fn paths() -> Paths {
    Paths {
        root: PathBuf::from(ROOT),
    }
}

fn env(sys: &dyn System) -> Env<'_> {
    Env {
        sys,
        paths: paths(),
        json: false,
        tty: false,
        parallel: Some(1),
        agent_sock: None,
    }
}

fn lab(n: u8, uuid: &str) -> Host {
    Host {
        name: format!("lab-0{n}"),
        uuid: uuid.into(),
        address: format!("192.168.64.{n}"),
        user: "admin".into(),
        baseline: None,
        users: Vec::new(),
    }
}

fn controller(hosts: &[&Host], github: bool) -> Fake {
    let sys = Fake::root();
    let github = if github {
        "[github]\naccount = \"fleetbot\"\n"
    } else {
        ""
    };
    names(&sys, hosts, github);
    sys.file(
        &enroll::own_exe().unwrap().to_string_lossy(),
        b"this controller's agent",
    );
    sys.file(
        &format!("{ROOT}/keys/signing.pub"),
        format!("ssh-ed25519 {SIGNING_BLOB} signing@old-name\n").as_bytes(),
    );
    sys.file(
        &format!("{ROOT}/allowed_signers"),
        format!(
            "signing@old-name namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n\
             recovery@old-name namespaces=\"aterm-machines\" ssh-ed25519 {RECOVERY_BLOB}\n"
        )
        .as_bytes(),
    );
    sys
}

/// Writes `machines.toml` naming `hosts`, with `github`'s table text (or none).
fn names(sys: &Fake, hosts: &[&Host], github: &str) {
    let mut inv = format!("schema = 1\n{github}");
    for h in hosts {
        inv.push_str(&format!(
            "[[host]]\nname = \"{}\"\nuuid = \"{}\"\naddress = \"{}\"\nuser = \"{}\"\n",
            h.name, h.uuid, h.address, h.user
        ));
    }
    sys.file(&format!("{ROOT}/machines.toml"), inv.as_bytes());
}

fn request(host: &Host, command: &str) -> String {
    ssh::request_cmd(&paths(), &Target::of(host), command)
        .unwrap()
        .display()
}

fn hello_of(host: &Host) -> Hello {
    Hello {
        schema: 1,
        agent_version: "0.106.0".into(),
        agent_sha256: "ab".repeat(32),
        uuid: host.uuid.clone(),
        serial_number: "ZNYVNTRJN5".into(),
        os_version: "27.0".into(),
        os_build: "26A428".into(),
        ..Hello::default()
    }
}

fn state_of(sys: &Fake, host: &Host) -> HostState {
    inventory::load_state(sys, &paths(), &host.uuid).unwrap()
}

fn row(id: &str, family: &str, outcome: Outcome) -> ItemReport {
    ItemReport {
        id: id.into(),
        family: family.into(),
        title: id.into(),
        outcome,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    }
}

fn report(mode: Mode, items: Vec<ItemReport>) -> HostReport {
    HostReport {
        schema: 1,
        mode,
        serial: 7,
        key_id: String::new(),
        run_id: "1800000000-7".into(),
        started: 1_799_999_000,
        finished: 1_799_999_900,
        facts: crate::model::HostFacts::default(),
        items,
        refused: None,
    }
}

fn refusal(why: &str) -> HostReport {
    let line = aterm_json::to_string(&Line::Refused { why: why.into() }).unwrap();
    read_stream(&format!("{line}\n"), 1_800_000_000).0.unwrap()
}

fn inventory_of(sys: &Fake) -> Inventory {
    inventory::load(sys, &paths()).unwrap()
}

fn inert(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

#[test]
fn the_starter_baseline_and_inventory_parse() {
    let b = crate::baseline::parse(templates::DEFAULT_BASELINE).unwrap();
    assert!(b.keyboard.is_some());
    assert!(b.homebrew.is_none() && b.apps.is_empty());
    let inv: Inventory = aterm_toml::from_str(templates::INVENTORY).unwrap();
    assert_eq!(inv.schema, 1);
    assert!(inv.hosts.is_empty());
}

fn uninitialized(protected: bool) -> Fake {
    let sys = Fake::root();
    sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok("ctl\n"));
    sys.stat_of(
        &format!("{ROOT}/keys/ctl"),
        crate::port::Stat {
            uid: 0,
            gid: 0,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    for (key, comment, blob, asked) in [
        (
            "ssh_ed25519",
            "aterm-machines-fleet@ctl",
            SIGNING_BLOB,
            false,
        ),
        ("signing", "signing@ctl", SIGNING_BLOB, protected),
        ("recovery", "recovery@ctl", RECOVERY_BLOB, protected),
    ] {
        let path = format!("{ROOT}/keys/{key}");
        let run = if asked {
            format!("tty /usr/bin/ssh-keygen -t ed25519 -C {comment} -f {path}")
        } else {
            format!("/usr/bin/ssh-keygen -q -t ed25519 -N '' -C {comment} -f {path}")
        };
        sys.on(&run, Ran::ok(""));
        sys.on_write(
            &run,
            Path::new(&format!("{path}.pub")),
            format!("ssh-ed25519 {blob} {comment}\n").as_bytes(),
        );
        if asked {
            sys.on(
                &format!("/usr/bin/ssh-keygen -y -P '' -f {path}"),
                Ran::exit(
                    255,
                    "Load key: incorrect passphrase supplied to decrypt private key\n",
                ),
            );
        }
    }
    sys
}

#[test]
fn init_makes_every_key_with_no_passphrase_asking_nothing_and_needing_no_terminal() {
    let sys = uninitialized(false);
    let mut out = Vec::new();
    assert_eq!(init(&env(&sys), false, &mut out), ExitCode::SUCCESS);
    let calls = sys.calls();
    for key in ["ssh_ed25519", "signing", "recovery"] {
        assert!(
            calls.iter().any(
                |c| c.starts_with("/usr/bin/ssh-keygen -q -t ed25519 -N '' ")
                    && c.ends_with(&format!("-f {ROOT}/keys/{key}"))
            ),
            "{key}: {calls:?}"
        );
    }
    assert!(!calls.iter().any(|c| c.starts_with("tty ")), "{calls:?}");
    let signers =
        String::from_utf8(sys.written(&format!("{ROOT}/allowed_signers")).unwrap()).unwrap();
    assert!(signers.starts_with("signing@ctl ") && signers.contains("\nrecovery@ctl "));
    let out = String::from_utf8(out).unwrap();
    assert!(
        !out.lines().any(|l| {
            let l = l.trim_start();
            l.split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }),
        "{out}"
    );
    assert_eq!(
        out.lines().last(),
        Some(
            format!("The controller is ready in {ROOT}. Next: aterm machines help enroll").as_str()
        )
    );
    assert!(!out.contains("removable media"), "{out}");
    let before = sys.calls().len();
    assert_eq!(init(&env(&sys), false, &mut Vec::new()), ExitCode::SUCCESS);
    assert!(
        !sys.calls()[before..]
            .iter()
            .any(|c| c.contains("ssh-keygen")),
        "{:?}",
        &sys.calls()[before..]
    );
}

#[test]
fn init_with_passphrase_asks_on_the_terminal_and_refuses_without_one() {
    let sys = uninitialized(true);
    assert_eq!(init(&env(&sys), true, &mut Vec::new()), ExitCode::from(2));
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    let mut e = env(&sys);
    e.tty = true;
    assert_eq!(init(&e, true, &mut Vec::new()), ExitCode::SUCCESS);
    let calls = sys.calls();
    let asked: Vec<&String> = calls.iter().filter(|c| c.starts_with("tty ")).collect();
    assert_eq!(asked.len(), 2, "the signing and recovery keys: {calls:?}");
    assert!(asked.iter().all(|c| !c.contains("-N")), "{asked:?}");
    assert!(
        calls
            .iter()
            .any(|c| c.contains("-N '' -C aterm-machines-fleet@ctl"))
    );
}

#[test]
fn init_with_passphrase_later_gives_one_to_a_signing_key_made_without() {
    let sys = uninitialized(false);
    assert_eq!(init(&env(&sys), false, &mut Vec::new()), ExitCode::SUCCESS);
    for key in ["ssh_ed25519", "signing"] {
        sys.file(&format!("{ROOT}/keys/{key}"), b"private");
    }
    let probe = format!("/usr/bin/ssh-keygen -y -P '' -f {ROOT}/keys/signing");
    sys.on(&probe, Ran::ok(&format!("ssh-ed25519 {SIGNING_BLOB}\n")));
    sys.on(
        &probe,
        Ran::exit(
            255,
            "Load key: incorrect passphrase supplied to decrypt private key\n",
        ),
    );
    let protect = format!("tty /usr/bin/ssh-keygen -p -f {ROOT}/keys/signing");
    sys.on(&protect, Ran::ok(""));
    let mut e = env(&sys);
    e.tty = true;
    let before = sys.calls().len();
    let mut out = Vec::new();
    assert_eq!(init(&e, true, &mut out), ExitCode::SUCCESS);
    let calls = sys.calls()[before..].to_vec();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("tty "))
            .collect::<Vec<_>>(),
        [&protect],
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("-t ed25519")), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.contains("keys/ssh_ed25519")),
        "{calls:?}"
    );
    assert!(String::from_utf8(out).unwrap().contains(&format!(
        "gave the signing key a passphrase: {ROOT}/keys/signing"
    )));
    let before = sys.calls().len();
    assert_eq!(init(&e, true, &mut Vec::new()), ExitCode::SUCCESS);
    assert!(
        !sys.calls()[before..].iter().any(|c| c.starts_with("tty ")),
        "{:?}",
        &sys.calls()[before..]
    );
}

#[test]
fn plan_with_no_host_plans_them_all_and_apply_with_none_is_refused() {
    let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
    let sys = controller(&[&a, &b], false);
    for h in [&a, &b] {
        sys.on(
            &request(h, "hello"),
            Ran::exit(
                255,
                &format!(
                    "ssh: connect to host {} port 22: Connection refused\r\n",
                    h.address
                ),
            ),
        );
    }
    let e = env(&sys);
    assert_eq!(
        plan_or_apply(&e, &[], false, Mode::Apply, false),
        ExitCode::from(2)
    );
    assert!(
        sys.calls().is_empty(),
        "apply named no host: {:?}",
        sys.calls()
    );
    assert_eq!(
        plan_or_apply(&e, &[], false, Mode::Plan, false),
        ExitCode::from(2)
    );
    let calls = sys.calls();
    for h in [&a, &b] {
        assert!(
            calls.contains(&request(h, "hello")),
            "{}: {calls:?}",
            h.name
        );
    }
}

#[test]
fn exit_status_says_failed_before_unreachable() {
    assert_eq!(Tally::default().code(), ExitCode::SUCCESS);
    assert_eq!(
        Tally {
            failed: false,
            unreachable: true
        }
        .code(),
        ExitCode::from(2)
    );
    assert_eq!(
        Tally {
            failed: true,
            unreachable: true
        }
        .code(),
        ExitCode::FAILURE
    );
}

#[test]
fn users_default_to_the_inventory_and_an_explicit_list_is_kept() {
    let host = Host {
        name: "lab-07".into(),
        uuid: "U".into(),
        address: "lab-07.local".into(),
        user: "labadmin".into(),
        baseline: None,
        users: Vec::new(),
    };
    let filled = default_users("schema = 1\n", &host).unwrap();
    assert_eq!(crate::baseline::parse(&filled).unwrap().users, ["labadmin"]);
    let kept = default_users("schema = 1\nusers = [\"alice\"]\n", &host).unwrap();
    assert_eq!(crate::baseline::parse(&kept).unwrap().users, ["alice"]);
}

#[test]
fn a_stream_ends_in_its_report_and_a_refusal_is_a_refused_report_stamped_now() {
    let r = refusal("replay principal=p serial=3 floor=9");
    assert!(r.refused.unwrap().starts_with("replay"));
    assert_eq!(r.finished, 1_800_000_000, "never read as decades ago");
    assert_eq!(read_stream("garbage\n", 1).0, None);
    let started = aterm_json::to_string(&Line::Started {
        run: "1800000000-7".into(),
    })
    .unwrap();
    assert_eq!(
        read_stream(&format!("{started}\n"), 1),
        (None, Some("1800000000-7".to_owned()))
    );
}

#[test]
fn ages_read_in_the_largest_unit() {
    assert_eq!(age(5), "5s");
    assert_eq!(age(120), "2m");
    assert_eq!(age(7200), "2h");
    assert_eq!(age(3 * 86_400), "3d");
}

#[test]
fn a_host_runs_another_agent_build_unless_it_reports_this_one() {
    let own = OwnAgent {
        exe: PathBuf::from("/x/aterm"),
        sha256: "ab".repeat(32),
    };
    let mut hello: Hello = aterm_json::from_str(
        r#"{"schema":1,"agent_version":"0.1.0","families":[],"uuid":"U","serial_number":"S",
                "computer_name":"c","local_host_name":"l","os_version":"27.0","os_build":"26A428",
                "arch":"arm64","floors":[],"cached":[]}"#,
    )
    .unwrap();
    assert!(own.differs(&hello));
    hello.agent_sha256 = "AB".repeat(32);
    assert!(!own.differs(&hello));
    hello.agent_sha256 = "cd".repeat(32);
    assert!(own.differs(&hello));
}

#[test]
fn the_floor_is_the_highest_any_of_the_keys_names_has_and_zero_for_a_new_one() {
    let hello: Hello = aterm_json::from_str(
        r#"{"schema":1,"agent_version":"0.1.0","families":[],"uuid":"U","serial_number":"S",
                "computer_name":"c","local_host_name":"l","os_version":"27.0","os_build":"26A428",
                "arch":"arm64","floors":[{"principal":"signing@ctl","serial":41},
                {"principal":"signing@renamed","serial":12}],"cached":[]}"#,
    )
    .unwrap();
    let names = |n: &[&str]| n.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(floor_of(&hello, &names(&["signing@ctl"])), 41);
    assert_eq!(
        floor_of(&hello, &names(&["signing@renamed", "signing@ctl"])),
        41
    );
    assert_eq!(floor_of(&hello, &names(&["recovery@ctl"])), 0);
}

#[test]
fn a_changed_host_key_fails_the_command_and_is_kept_with_its_reason_not_as_unreachable() {
    let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
    let sys = controller(&[&a, &b], false);
    sys.on(&request(&a, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
    sys.on(
        &request(&b, "hello"),
        Ran::exit(
            255,
            "ssh: connect to host 192.168.64.2 port 22: Operation timed out\r\n",
        ),
    );
    let e = env(&sys);
    assert_eq!(
        plan_or_apply(&e, &[], true, Mode::Apply, false),
        ExitCode::FAILURE,
        "a possible impostor is never the harmless exit 2"
    );
    let changed = state_of(&sys, &a);
    assert_eq!(changed.unreachable_since, None);
    let (since, why) = changed.contact_error.clone().unwrap();
    assert_eq!(since, sys.now);
    assert!(why.contains("host key is not the one pinned"), "{why}");
    assert_eq!(state_of(&sys, &b).unreachable_since, Some(sys.now));
    assert_eq!(state_of(&sys, &b).contact_error, None);
    assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
    assert_eq!(outstanding(&e), ExitCode::FAILURE);
    assert!(
        summary(&a, &changed, sys.now + 3600).contains("failing for 1h: its host key"),
        "{}",
        summary(&a, &changed, sys.now + 3600)
    );
}

#[test]
fn hosts_that_only_could_not_be_reached_exit_2_and_another_mac_answering_fails() {
    let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
    let sys = controller(&[&a, &b], false);
    sys.on(
        &request(&a, "hello"),
        Ran::exit(
            255,
            "ssh: connect to host 192.168.64.1 port 22: Connection refused\r\n",
        ),
    );
    sys.on(
        &request(&b, "hello"),
        Ran::exit(
            255,
            "ssh: connect to host 192.168.64.2 port 22: Operation timed out\r\n",
        ),
    );
    sys.on(
        &request(&b, "hello"),
        Ran::ok(&aterm_json::to_string(&hello_of(&a)).unwrap()),
    );
    let e = env(&sys);
    assert_eq!(
        plan_or_apply(&e, &[], true, Mode::Plan, false),
        ExitCode::from(2)
    );
    assert_eq!(
        plan_or_apply(&e, &[], true, Mode::Plan, false),
        ExitCode::FAILURE
    );
    assert!(
        state_of(&sys, &b)
            .contact_error
            .unwrap()
            .1
            .contains("this is not lab-02")
    );
}

/// Where no host answered and the LAN broker made no connection, the sweep stops on the
/// broker's words, once; a host that answered, or failures that are the hosts' own, keep
/// the per-host rows. NEGATIVE CONTROL: a wall on any broker failure stops a sweep a host
/// answered.
#[test]
fn the_broker_line_stands_for_the_hosts_only_when_none_answered() {
    let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
    let reached = |ha: Result<Hello, HelloError>, hb: Result<Hello, HelloError>| {
        broker_wall(&[
            Reached {
                host: Cow::Borrowed(&a),
                hello: ha,
            },
            Reached {
                host: Cow::Borrowed(&b),
                hello: hb,
            },
        ])
    };
    let said = format!(
        "{}{}",
        super::super::lan_proxy::BROKER_SAID,
        aterm_uds::lan::Unavailable::NotInstalled
    );
    let broker = || Err(HelloError::Ssh(ssh::Failure::Broker(said.clone())));
    assert_eq!(reached(Ok(hello_of(&a)), broker()), None);
    assert_eq!(
        reached(broker(), broker()),
        Some(format!("controller: {said}"))
    );
    assert_eq!(
        reached(broker(), Err(HelloError::Ssh(ssh::Failure::Refused))),
        Some(format!("controller: {said}"))
    );
    assert_eq!(
        reached(
            Err(HelloError::Ssh(ssh::Failure::NoRoute)),
            Err(HelloError::Ssh(ssh::Failure::TimedOut))
        ),
        None
    );
}

#[test]
fn an_unreadable_inventory_is_a_failure_and_all_over_no_hosts_is_nothing_to_do() {
    let sys = Fake::root();
    sys.file(
        &format!("{ROOT}/machines.toml"),
        b"schema = 1\n[[host]]\nname = 3\n",
    );
    let e = env(&sys);
    assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
    assert_eq!(
        plan_or_apply(&e, &[], true, Mode::Plan, false),
        ExitCode::FAILURE
    );
    assert_eq!(outstanding(&e), ExitCode::FAILURE);
    let missing = Fake::root();
    assert_eq!(status(&env(&missing), &[], true, false), ExitCode::FAILURE);
    let empty = controller(&[], false);
    assert_eq!(
        plan_or_apply(&env(&empty), &[], true, Mode::Apply, false),
        ExitCode::SUCCESS
    );
    assert_eq!(
        plan_or_apply(&env(&empty), &[], false, Mode::Apply, false),
        ExitCode::from(2)
    );
}

#[test]
fn two_macs_their_baseline_would_give_one_local_hostname_are_sent_nothing() {
    let m1 = Host {
        name: "m1".into(),
        ..lab(1, UUID_1)
    };
    let spare = Host {
        name: "m1-spare".into(),
        ..lab(2, UUID_2)
    };
    let sys = controller(&[&m1, &spare], false);
    sys.file(
        &format!("{ROOT}/baselines/default.toml"),
        b"schema = 1\n[names]\ncomputer_name = \"{tag} {hardware}\"\nlocal_host_name = \"{tag}-{hardware}\"\n",
    );
    for h in [&m1, &spare] {
        sys.on(
            &request(h, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(h)).unwrap()),
        );
    }
    assert_eq!(
        plan_or_apply(&env(&sys), &[], true, Mode::Plan, false),
        ExitCode::FAILURE
    );
    for h in [&m1, &spare] {
        assert!(!sys.calls().contains(&request(h, "apply")));
    }
    assert!(
        sys.written(&format!("{ROOT}/serial")).is_none(),
        "no serial taken"
    );
}

#[test]
fn a_mac_renamed_at_its_dot_local_address_is_found_there_next_time() {
    let mut m1 = lab(1, UUID_1);
    m1.address = "m1-Air-M2-24GB-1TB-2.local".into();
    let sys = controller(&[&m1], false);
    let mut renamed = row("names.local-host-name", "names", Outcome::Changed);
    renamed.from = Some("\"m1-Air-M2-24GB-1TB-2\"".into());
    renamed.to = Some("\"m1-MacBook-Air-M2-24GB-1TB\"".into());
    let moved = follow_rename(&env(&sys), &m1, &report(Mode::Apply, vec![renamed])).unwrap();
    assert_eq!(moved.address, "m1-MacBook-Air-M2-24GB-1TB.local");
    let inv = inventory_of(&sys);
    assert_eq!(inv.hosts[0].address, "m1-MacBook-Air-M2-24GB-1TB.local");
    assert_eq!(inv.hosts[0].name, "lab-01");
}

fn renamed_vm() -> (Fake, Host, Host) {
    let vm = Host {
        name: "lab-03".into(),
        address: "lab-03.local".into(),
        ..lab(3, UUID_1)
    };
    let sys = controller(&[&vm], false);
    sys.file(
        &format!("{ROOT}/baselines/default.toml"),
        b"schema = 1\n[names]\ncomputer_name = \"{tag} {hardware}\"\nlocal_host_name = \"{tag}-{hardware}\"\n",
    );
    let mut said = hello_of(&vm);
    said.hardware = ["Virtual Machine", "M5", "8GB", "90GB"]
        .map(String::from)
        .to_vec();
    let state = HostState {
        hello: Some(aterm_json::to_string(&said).unwrap()),
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &vm.uuid, &state).unwrap();
    let moved = Host {
        address: "lab-03-Virtual-Machine-M5-8GB-90GB.local".into(),
        ..vm.clone()
    };
    (sys, vm, moved)
}

const UNRESOLVED_OLD: &str = "ssh: Could not resolve hostname lab-03.local: nodename nor servname provided, or not known\r\n";

#[test]
fn a_mac_renamed_by_a_run_whose_report_was_lost_is_found_at_the_name_its_baseline_gives() {
    let (sys, vm, moved) = renamed_vm();
    sys.on(&request(&vm, "hello"), Ran::exit(255, UNRESOLVED_OLD));
    let mut answer = hello_of(&vm);
    answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
    sys.on(
        &request(&moved, "hello"),
        Ran::ok(&aterm_json::to_string(&answer).unwrap()),
    );
    let finished = report(Mode::Apply, Vec::new());
    let stream = aterm_json::to_string(&Line::Report(Box::new(finished.clone()))).unwrap();
    sys.on(
        &request(&moved, "attach 1800000000-7"),
        Ran::ok(&format!("{stream}\n")),
    );
    let e = env(&sys);
    assert_eq!(
        attach(&e, "lab-03", Some("1800000000-7")),
        ExitCode::SUCCESS
    );
    assert_eq!(inventory_of(&sys).hosts[0].address, moved.address);
    assert!(!sys.calls().contains(&request(&vm, "attach 1800000000-7")));
    let state = state_of(&sys, &vm);
    assert_eq!((state.last_seen, state.unreachable_since), (sys.now, None));
    assert_eq!(state.report, Some(finished));
    let at_old = |sys: &Fake| {
        sys.calls()
            .iter()
            .filter(|c| **c == request(&vm, "hello"))
            .count()
    };
    let asked = at_old(&sys);
    let inv = inventory_of(&sys);
    let reached = reach_one(&e, &inv.hosts[0]);
    assert!(reached.hello.is_ok());
    assert_eq!(at_old(&sys), asked);
}

#[test]
fn a_mac_still_reached_by_its_old_name_moves_to_the_one_it_says_it_has_once_that_answers() {
    let (sys, vm, moved) = renamed_vm();
    let mut answer = hello_of(&vm);
    answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
    let answer = Ran::ok(&aterm_json::to_string(&answer).unwrap());
    sys.on(&request(&vm, "hello"), answer.clone())
        .on(&request(&moved, "hello"), answer);
    let reached = reach_one(&env(&sys), &vm);
    assert!(reached.hello.is_ok());
    assert_eq!(reached.host.address, moved.address);
    assert_eq!(inventory_of(&sys).hosts[0].address, moved.address);
    assert_eq!(found_lead(&vm), format!("{:<16} renamed: ", vm.name));
    let (sys, vm, moved) = renamed_vm();
    let mut answer = hello_of(&vm);
    answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
    sys.on(
        &request(&vm, "hello"),
        Ran::ok(&aterm_json::to_string(&answer).unwrap()),
    )
    .on(
        &request(&moved, "hello"),
        Ran::exit(
            255,
            &UNRESOLVED_OLD.replace("lab-03.local", "lab-03-Virtual-Machine-M5-8GB-90GB.local"),
        ),
    );
    let reached = reach_one(&env(&sys), &vm);
    assert!(reached.hello.is_ok());
    assert!(matches!(reached.host, Cow::Borrowed(_)));
    assert_eq!(inventory_of(&sys).hosts[0].address, "lab-03.local");
    assert_eq!(state_of(&sys, &vm).contact_error, None);
}

#[test]
fn another_mac_at_the_renamed_name_is_never_followed_and_nothing_else_is_tried() {
    let (sys, vm, moved) = renamed_vm();
    sys.on(&request(&vm, "hello"), Ran::exit(255, UNRESOLVED_OLD));
    let other = lab(9, UUID_2);
    sys.on(
        &request(&moved, "hello"),
        Ran::ok(&aterm_json::to_string(&hello_of(&other)).unwrap()),
    )
    .on(&request(&moved, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
    let e = env(&sys);
    for _ in 0..2 {
        assert_eq!(
            attach(&e, "lab-03", Some("1800000000-7")),
            ExitCode::from(2)
        );
        assert_eq!(inventory_of(&sys).hosts[0].address, "lab-03.local");
        let state = state_of(&sys, &vm);
        assert_eq!(state.unreachable_since, Some(sys.now));
        assert_eq!(state.contact_error, None);
    }
    assert!(!sys.calls().iter().any(|c| c.contains("attach ")));
    let (sys, vm, moved) = renamed_vm();
    sys.on(
        &request(&vm, "hello"),
        Ran::exit(
            255,
            "ssh: connect to host lab-03.local port 22: Operation timed out\r\n",
        ),
    );
    let reached = reach_one(&env(&sys), &vm);
    assert!(matches!(reached.host, Cow::Borrowed(_)));
    assert!(!sys.calls().contains(&request(&moved, "hello")));
}

#[test]
fn a_mac_whose_old_name_another_mac_took_is_found_at_the_name_its_baseline_gives() {
    let (sys, vm, moved) = renamed_vm();
    sys.on(&request(&vm, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
    let mut answer = hello_of(&vm);
    answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
    sys.on(
        &request(&moved, "hello"),
        Ran::ok(&aterm_json::to_string(&answer).unwrap()),
    );
    let reached = reach_one(&env(&sys), &vm);
    assert!(reached.hello.is_ok());
    assert_eq!(reached.host.address, moved.address);
    assert_eq!(inventory_of(&sys).hosts[0].address, moved.address);
    let state = state_of(&sys, &vm);
    assert_eq!((state.contact_error, state.last_seen), (None, sys.now));
    let (sys, vm, moved) = renamed_vm();
    sys.on(&request(&vm, "hello"), Ran::exit(255, HOST_KEY_CHANGED))
        .on(
            &request(&moved, "hello"),
            Ran::exit(
                255,
                &UNRESOLVED_OLD.replace("lab-03.local", "lab-03-Virtual-Machine-M5-8GB-90GB.local"),
            ),
        );
    let reached = reach_one(&env(&sys), &vm);
    assert_eq!(
        reached.hello,
        Err(HelloError::Ssh(ssh::Failure::HostKeyChanged))
    );
    assert!(matches!(reached.host, Cow::Borrowed(_)));
    assert_eq!(inventory_of(&sys).hosts[0].address, "lab-03.local");
    assert!(
        state_of(&sys, &vm)
            .contact_error
            .is_some_and(|(_, why)| why.contains("host key"))
    );
}

#[test]
fn an_upgraded_agent_is_greeted_before_the_baseline_that_may_rename_the_mac() {
    let vm = Host {
        name: "lab-03".into(),
        address: "lab-03.local".into(),
        ..lab(3, UUID_1)
    };
    let sys = controller(&[&vm], false);
    let archive = enroll::Archive {
        path: PathBuf::from("/scratch/agent.zip"),
        sha256: "cd".repeat(32),
        exe: PathBuf::from("aterm"),
        bundle: false,
    };
    let mut old = hello_of(&vm);
    old.cached = vec![archive.sha256.clone()];
    let stream = |items: Vec<ItemReport>| {
        let line = Line::Report(Box::new(report(Mode::Apply, items)));
        Ran::ok(&format!("{}\n", aterm_json::to_string(&line).unwrap()))
    };
    sys.on(
        &request(&vm, "apply"),
        stream(vec![row("agent.upgrade", "agent", Outcome::Changed)]),
    )
    .on(
        &request(&vm, "apply"),
        stream(vec![row("names.local-host-name", "names", Outcome::Ok)]),
    );
    let mut new = hello_of(&vm);
    new.agent_sha256 = "ef".repeat(32);
    new.hardware = ["Virtual Machine", "M5", "8GB", "90GB"]
        .map(String::from)
        .to_vec();
    sys.on(
        &request(&vm, "hello"),
        Ran::ok(&aterm_json::to_string(&new).unwrap()),
    );
    let job = Job {
        host: Cow::Borrowed(&vm),
        hello: old,
        needed: Vec::new(),
        upgrade: Some(b"upgrade".to_vec()),
        wire: b"baseline".to_vec(),
    };
    let done = send(&env(&sys), &job, Some(&archive)).unwrap();
    assert_eq!(done.items.len(), 2);
    let calls = sys.calls();
    let at = |cmd: String| -> Vec<usize> {
        calls
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == cmd)
            .map(|(i, _)| i)
            .collect()
    };
    let (applies, hellos) = (at(request(&vm, "apply")), at(request(&vm, "hello")));
    assert_eq!((applies.len(), hellos.len()), (2, 1), "{calls:?}");
    assert!(
        applies[0] < hellos[0] && hellos[0] < applies[1],
        "{calls:?}"
    );
    let kept: Hello = aterm_json::from_str(state_of(&sys, &vm).hello.as_deref().unwrap()).unwrap();
    assert_eq!(kept, new);
}

#[test]
fn a_busy_host_is_sent_nothing_and_fails_the_command() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    let mut busy = hello_of(&a);
    busy.busy = Some("1799999999-4".into());
    sys.on(
        &request(&a, "hello"),
        Ran::ok(&aterm_json::to_string(&busy).unwrap()),
    );
    assert_eq!(
        plan_or_apply(&env(&sys), &[], true, Mode::Apply, false),
        ExitCode::FAILURE
    );
    assert!(!sys.calls().contains(&request(&a, "apply")));
    assert!(
        sys.written(&format!("{ROOT}/serial")).is_none(),
        "no serial taken"
    );
}

#[test]
fn an_only_envelope_carries_those_tables_and_resolves_nothing_for_the_others() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    sys.file(
        &format!("{ROOT}/baselines/default.toml"),
        templates::DEFAULT_BASELINE.as_bytes(),
    );
    let e = env(&sys);
    let text = resolved_baseline(
        &e,
        &a,
        &hello_of(&a),
        &mut Resolved::default(),
        &["names".to_owned()],
    )
    .unwrap();
    let b = crate::baseline::parse(&text).unwrap();
    assert!(b.names.is_some());
    assert!(b.aterm.is_none() && b.apps.is_empty() && b.homebrew.is_none());
    assert!(b.power.is_none() && b.keyboard.is_none() && b.developer.is_none());
    assert_eq!(
        b.users,
        std::slice::from_ref(&a.user),
        "the users stay for user-scope names"
    );
    assert!(
        sys.calls().is_empty(),
        "no release looked up: {:?}",
        sys.calls()
    );
    assert!(resolved_baseline(&e, &a, &hello_of(&a), &mut Resolved::default(), &[]).is_err());
}

#[test]
fn an_only_apply_names_what_the_cache_holds_so_the_agent_prunes_none_of_it() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    sys.file(
        &format!("{ROOT}/baselines/default.toml"),
        templates::DEFAULT_BASELINE.as_bytes(),
    );
    let e = env(&sys);
    let (app, pin) = ("ab".repeat(32), "CD".repeat(32));
    let mut hello = hello_of(&a);
    hello.cached = vec![app.clone(), pin.clone(), "../x".into()];
    let only = ["names".to_owned()];
    let body = |action: Action| {
        baseline_body(
            &e,
            &a,
            &hello,
            0,
            action,
            false,
            &mut Resolved::default(),
            &only,
        )
        .unwrap()
    };
    let (apply, needed) = body(Action::Apply);
    assert!(needed.is_empty(), "nothing to upload for [names]");
    let named: Vec<&str> = apply.artifacts.iter().map(|x| x.sha256.as_str()).collect();
    assert_eq!(named, [app.as_str(), &pin.to_ascii_lowercase()]);
    let (plan, _) = body(Action::Plan);
    assert!(plan.artifacts.is_empty(), "{:?}", plan.artifacts);
}

#[test]
fn an_only_run_whose_state_cannot_note_it_is_not_sent() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    sys.file(
        &format!("{ROOT}/baselines/default.toml"),
        templates::DEFAULT_BASELINE.as_bytes(),
    );
    sys.file(&format!("{ROOT}/state/{UUID_1}.json"), b"{\"report\":");
    sys.on(
        &request(&a, "hello"),
        Ran::ok(&aterm_json::to_string(&hello_of(&a)).unwrap()),
    );
    let e = env(&sys);
    assert_eq!(
        plan_or_apply_only(&e, &[], true, Mode::Plan, false, &["names".to_owned()]),
        ExitCode::FAILURE
    );
    let calls = sys.calls();
    assert!(!calls.iter().any(|c| c.contains("ssh-keygen")), "{calls:?}");
    assert_eq!(
        sys.written(&format!("{ROOT}/state/{UUID_1}.json")),
        Some(b"{\"report\":".to_vec()),
        "the state is left as it was"
    );
}

fn whole_apply() -> HostReport {
    let mut brew = row("homebrew.pkg", "homebrew", Outcome::Changed);
    brew.effect = Effect::Restart;
    let mut r = report(
        Mode::Apply,
        vec![
            row("names.computer-name", "names", Outcome::Failed),
            row("power.ac.sleep", "power", Outcome::Failed),
            brew,
        ],
    );
    r.facts.os_build = "26A428".into();
    r
}

fn names_plan(serial: u64) -> HostReport {
    let mut r = report(
        Mode::Plan,
        vec![
            row("agent.upgrade", "agent", Outcome::Planned),
            row("names.computer-name", "names", Outcome::Planned),
            row("names.local-host-name", "names", Outcome::Planned),
        ],
    );
    r.serial = serial;
    r.finished = 1_800_000_000;
    r.facts.os_build = "26B5".into();
    r
}

#[test]
fn an_only_report_replaces_its_tables_rows_and_leaves_the_others_as_their_run_left_them() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    after_run(&e, &inv, &a, whole_apply(), Source::Baseline);
    remember_only(&e, &a, 8, &["names".to_owned()]).unwrap();
    after_run(&e, &inv, &a, names_plan(8), Source::Baseline);
    let state = state_of(&sys, &a);
    let saved = state.report.as_ref().unwrap();
    let rows: Vec<(&str, Outcome)> = saved
        .items
        .iter()
        .map(|r| (r.id.as_str(), r.outcome))
        .collect();
    assert_eq!(
        rows,
        [
            ("agent.upgrade", Outcome::Planned),
            ("names.computer-name", Outcome::Planned),
            ("names.local-host-name", Outcome::Planned),
            ("power.ac.sleep", Outcome::Failed),
            ("homebrew.pkg", Outcome::Changed),
        ]
    );
    assert_eq!((saved.mode, saved.serial), (Mode::Plan, 8));
    assert_eq!(
        state.only,
        Some(inventory::Only {
            tables: vec!["names".into()],
            rest: Some(1_799_999_900),
        })
    );
    assert!(state.only_sent.is_empty(), "the serial is spent");
    remember_only(&e, &a, 9, &["names".to_owned()]).unwrap();
    let mut names_apply = names_plan(9);
    names_apply.mode = Mode::Apply;
    after_run(&e, &inv, &a, names_apply, Source::Baseline);
    let state = state_of(&sys, &a);
    assert_eq!(state.applied_build.as_deref(), Some("26A428"));
    assert_eq!(outstanding(&e), ExitCode::FAILURE);
    let line = summary(&a, &state, sys.now);
    assert!(line.contains("; names only (the rest "), "{line}");
    assert!(only_todo(&a, &state).is_none(), "the rest was run once");
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Plan, Vec::new()),
        Source::Baseline,
    );
    let state = state_of(&sys, &a);
    assert_eq!(state.only, None);
    assert!(state.report.unwrap().items.is_empty());
}

#[test]
fn a_host_only_only_runs_reached_says_the_rest_was_never_run() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    remember_only(&e, &a, 8, &["names".to_owned()]).unwrap();
    after_run(&e, &inv, &a, names_plan(8), Source::Followed);
    let state = state_of(&sys, &a);
    assert_eq!(
        state.only,
        Some(inventory::Only {
            tables: vec!["names".into()],
            rest: None,
        })
    );
    assert_eq!(
        only_todo(&a, &state).as_deref(),
        Some(
            "lab-01: only its names so far (--only); the rest of its baseline (apps, \
             settings) has not run yet: `aterm machines apply lab-01` does it when you want"
        )
    );
    let line = summary(&a, &state, sys.now);
    assert!(
        line.contains("; names only (the rest of the baseline not yet)"),
        "{line}"
    );
    remember_only(&e, &a, 9, &["power".to_owned()]).unwrap();
    let mut power = names_plan(9);
    power.items = vec![row("power.ac.sleep", "power", Outcome::Ok)];
    after_run(&e, &inv, &a, power, Source::Baseline);
    let state = state_of(&sys, &a);
    assert_eq!(
        state.only.map(|o| o.tables),
        Some(vec!["names".to_owned(), "power".to_owned()])
    );
    let ids: Vec<String> = state
        .report
        .unwrap()
        .items
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        ids,
        [
            "names.computer-name",
            "names.local-host-name",
            "power.ac.sleep"
        ]
    );
}

#[test]
fn a_refused_run_is_kept_beside_the_last_report_never_in_its_place() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    let applied = report(
        Mode::Apply,
        vec![row(
            "person.full_disk_access:aterm",
            "person",
            Outcome::Person,
        )],
    );
    after_run(&e, &inv, &a, applied.clone(), Source::Baseline);
    after_run(
        &e,
        &inv,
        &a,
        refusal("busy run=1800000000-9"),
        Source::Baseline,
    );
    let state = state_of(&sys, &a);
    assert_eq!(state.report.as_ref(), Some(&applied));
    assert_eq!(
        state.last_refusal,
        Some((sys.now, "busy run=1800000000-9".to_owned()))
    );
    assert!(summary(&a, &state, sys.now).contains("REFUSED 0s ago: busy run="));
    assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
    assert_eq!(outstanding(&e), ExitCode::FAILURE);
    assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Plan, Vec::new()),
        Source::Baseline,
    );
    assert_eq!(state_of(&sys, &a).last_refusal, None);
}

#[test]
fn an_exec_or_upgrade_report_attach_followed_leaves_the_hosts_report_alone() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    let applied = report(
        Mode::Apply,
        vec![row("power.ac.sleep", "power", Outcome::Failed)],
    );
    after_run(&e, &inv, &a, applied.clone(), Source::Baseline);
    for job in [
        report(Mode::Apply, vec![row("exec", "exec", Outcome::Changed)]),
        report(
            Mode::Apply,
            vec![row("agent.upgrade", "agent", Outcome::Changed)],
        ),
    ] {
        after_run(&e, &inv, &a, job, Source::Followed);
        assert_eq!(state_of(&sys, &a).report.as_ref(), Some(&applied));
    }
    let later = report(
        Mode::Apply,
        vec![row("power.ac.sleep", "power", Outcome::Changed)],
    );
    after_run(&e, &inv, &a, later.clone(), Source::Followed);
    assert_eq!(state_of(&sys, &a).report, Some(later));
}

#[test]
fn a_report_whose_os_build_moved_since_the_last_apply_reads_os_changed_with_the_agent() {
    let a = lab(1, UUID_1);
    let mut hello = hello_of(&a);
    hello.os_build = "26B5".into();
    let state = HostState {
        hello: aterm_json::to_string(&hello).ok(),
        last_seen: 1_800_000_000,
        report: Some(report(Mode::Apply, Vec::new())),
        applied_build: Some("26A428".into()),
        ..HostState::default()
    };
    let line = summary(&a, &state, 1_800_000_000);
    assert!(line.contains("os-changed (26A428 \u{2192} 26B5)"), "{line}");
    assert!(line.ends_with(", agent 0.106.0]"), "{line}");
    assert!(
        !line.contains("abababababab"),
        "the build's hash is --json's: {line}"
    );
    let unknown = HostState {
        applied_build: None,
        ..state.clone()
    };
    assert_eq!(os_changed(&unknown, saved_hello(&unknown).as_ref()), None);
    let same = HostState {
        applied_build: Some("26B5".into()),
        ..state
    };
    assert_eq!(os_changed(&same, saved_hello(&same).as_ref()), None);
}

#[test]
fn a_report_row_reads_by_its_title_never_its_id() {
    let mut r = row("names.netbios-name", "names", Outcome::Changed);
    r.title = "Windows network name, the Computer Name cut to 15 characters".into();
    r.from = Some("\"VIRTUALMAC-6C5B\"".into());
    r.to = Some("\"M201-VIRTUAL-MA\"".into());
    assert_eq!(
        row_line(&r),
        "changed   Windows network name, the Computer Name cut to 15 characters: \
         \"VIRTUALMAC-6C5B\" → \"M201-VIRTUAL-MA\""
    );
    r.outcome = Outcome::Failed;
    r.why = Some("scutil: Permission denied.".into());
    assert!(
        row_line(&r).ends_with("→ \"M201-VIRTUAL-MA\"; scutil: Permission denied."),
        "{}",
        row_line(&r)
    );
    r.from = None;
    assert!(
        row_line(&r).ends_with("15 characters: scutil: Permission denied."),
        "{}",
        row_line(&r)
    );
}

#[test]
fn what_a_host_sends_reaches_the_terminal_without_control_characters() {
    let osc52 = "\u{1b}]52;c;aGVsbG8=\u{7}";
    let mut r = row("defaults.user.x@alice", "defaults", Outcome::Planned);
    r.from = Some(format!("{{{osc52} = 1}}"));
    r.to = Some("{}".into());
    r.why = Some(format!("why{osc52}"));
    assert!(inert(&row_line(&r)), "{}", row_line(&r));
    let mut ran = report(Mode::Apply, vec![row("exec", "exec", Outcome::Changed)]);
    ran.items[0].to = Some("exit 0".into());
    ran.items[0].detail = Some(format!("file-{osc52}\r\nsecond\n"));
    ran.items[0].why = Some("\u{1b}[2J".to_owned());
    let lines = exec_lines(&ran);
    assert!(lines.iter().all(|l| inert(l)), "{lines:?}");
    assert_eq!(lines.len(), 4, "{lines:?}");
    let a = lab(1, UUID_1);
    let state = HostState {
        report: Some(refusal(&format!("busy {osc52}"))),
        last_refusal: Some((1, format!("busy {osc52}"))),
        contact_error: Some((1, osc52.to_owned())),
        ..HostState::default()
    };
    assert!(inert(&summary(&a, &state, 2)), "{}", summary(&a, &state, 2));
}

#[test]
fn a_person_row_nobody_could_read_is_listed_unmeasured_and_fails_person() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    let e = env(&sys);
    let mut unread = row("person.screen_sharing", "person", Outcome::Unknown);
    unread.why = Some("launchctl print-disabled: timed out".into());
    let state = HostState {
        report: Some(report(Mode::Plan, vec![unread])),
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
    assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
    let old = HostState {
        report: Some(refusal("expired issued=1 host-clock=2")),
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &a.uuid, &old).unwrap();
    assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
    let done = HostState {
        report: Some(report(
            Mode::Plan,
            vec![row("person.screen_sharing", "person", Outcome::Ok)],
        )),
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &a.uuid, &done).unwrap();
    assert_eq!(person(&e, &[], true), ExitCode::SUCCESS);
}

fn person_report(aterm_app: Option<(&str, Outcome)>) -> HostReport {
    let mut items = vec![
        row("person.screen_sharing", "person", Outcome::Person),
        row("person.full_disk_access:aterm", "person", Outcome::Person),
        row("person.developer_tools:aterm", "person", Outcome::Person),
    ];
    if let Some((from, outcome)) = aterm_app {
        let mut app = row("aterm.app", "aterm", outcome);
        app.from = Some(from.into());
        app.to = Some("aterm 0.106.0 (team A66A9P66Z7), owned by admin".into());
        items.insert(0, app);
    }
    report(Mode::Plan, items)
}

#[test]
fn screen_is_refused_while_screen_sharing_is_outstanding_whichever_step_is_named() {
    let a = lab(1, UUID_1);
    let r = person_report(None);
    let err = walk_steps(&r, &a, false, Some("developer_tools:aterm")).unwrap_err();
    assert!(err.contains("Screen Sharing is not on"), "{err}");
    let (steps, waiting) = walk_steps(&r, &a, true, None).unwrap();
    assert_eq!(steps.len(), 3);
    assert!(waiting.is_empty());
}

#[test]
fn before_apply_installs_aterm_its_switches_wait_and_the_rest_are_walked() {
    let a = lab(1, UUID_1);
    let (steps, waiting) = walk_steps(
        &person_report(Some(("absent", Outcome::Planned))),
        &a,
        true,
        None,
    )
    .unwrap();
    assert_eq!(
        steps.iter().map(screen::Step::id).collect::<Vec<_>>(),
        ["screen_sharing"]
    );
    assert_eq!(waiting.len(), 2);
    for w in &waiting {
        assert!(
            matches!(&w.done, screen::Done::NotGranted(why) if why.contains("aterm machines apply lab-01")),
            "{w:?}"
        );
    }
    for installed in [
        ("absent", Outcome::Changed),
        ("aterm 0.105.0", Outcome::Planned),
    ] {
        let (steps, waiting) = walk_steps(&person_report(Some(installed)), &a, true, None).unwrap();
        assert_eq!(steps.len(), 3, "{installed:?}");
        assert!(waiting.is_empty());
    }
}

#[test]
fn a_walk_plans_again_when_a_step_reads_granted_even_one_granted_before_it() {
    let a = lab(1, UUID_1);
    let walked = |done: screen::Done| {
        vec![screen::Walked {
            step: "full_disk_access:aterm".into(),
            done,
        }]
    };
    let told = |done| tell(&a, &walked(done), &mut |_| {});
    assert_eq!(told(screen::Done::AlreadyGranted), (false, true));
    assert_eq!(told(screen::Done::Granted), (false, true));
    assert_eq!(told(screen::Done::NotGranted("hang".into())), (true, false));
    assert_eq!(
        told(screen::Done::AtConsole("at the Mac".into())),
        (false, false)
    );
}

#[test]
fn a_mac_left_unlocked_is_the_walks_last_line_and_fails_it_whatever_the_steps_did() {
    let a = lab(1, UUID_1);
    let walked = |done: screen::Done| {
        vec![screen::Walked {
            step: "full_disk_access:aterm".into(),
            done,
        }]
    };
    let ended = |walked: &[screen::Walked], session: &screen::Session| {
        let mut said = Vec::new();
        let code = end_walk(&a, "screen", walked, session, Box::new(()), &mut |s| {
            said.push(s);
        });
        (code, said)
    };
    let unlocked = |why: &str| screen::Session::Unlocked {
        why: why.into(),
        unread: None,
        watched: false,
    };
    let line = "aterm machines screen: lab-01: the Mac is still logged in as admin and \
                unlocked at its display: the lock after the walk failed \
                (SACLockScreenImmediate answered 22)";
    assert_eq!(
        ended(
            &walked(screen::Done::Granted),
            &unlocked("SACLockScreenImmediate answered 22")
        ),
        (
            ExitCode::FAILURE,
            vec![
                Said::Step("lab-01: full_disk_access:aterm: granted".into()),
                Said::Plan,
                Said::Last(line.into()),
            ]
        )
    );
    let stopped = walked(screen::Done::NotGranted("stopped by SIGINT".into()));
    let (code, said) = ended(&stopped, &unlocked("SACLockScreenImmediate answered 22"));
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(said.last(), Some(&Said::Last(line.into())));
    assert_eq!(said.len(), 2);
    for session in [screen::Session::Locked, screen::Session::Kept] {
        assert_eq!(
            ended(&walked(screen::Done::Granted), &session),
            (
                ExitCode::SUCCESS,
                vec![
                    Said::Step("lab-01: full_disk_access:aterm: granted".into()),
                    Said::Plan,
                ]
            )
        );
        assert_eq!(ended(&stopped, &session).0, ExitCode::FAILURE);
    }
    let (_, said) = ended(&stopped, &unlocked("\u{1b}]52;c;aGVsbG8=\u{7}"));
    assert!(
        matches!(said.last(), Some(Said::Last(l)) if inert(l)),
        "{said:?}"
    );
}

struct Held(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.borrow_mut().push("stops let go".into());
    }
}

#[test]
fn the_stops_screen_caught_are_let_go_only_after_the_last_line() {
    let a = lab(1, UUID_1);
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let walked = vec![screen::Walked {
        step: "full_disk_access:aterm".into(),
        done: screen::Done::Granted,
    }];
    let session = screen::Session::Unlocked {
        why: "SACLockScreenImmediate answered 22".into(),
        unread: None,
        watched: true,
    };
    let held = Box::new(Held(std::rc::Rc::clone(&log)));
    let code = end_walk(&a, "screen", &walked, &session, held, &mut |said| {
        log.borrow_mut().push(format!("{said:?}"));
    });
    assert_eq!(code, ExitCode::FAILURE);
    let log = log.borrow();
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(log[1] == "Plan" && log[2].starts_with("Last("), "{log:?}");
    assert_eq!(log[3], "stops let go");
}

#[test]
fn a_key_with_a_passphrase_that_ssh_agent_holds_signs_through_it_without_a_prompt() {
    let sys = Fake::root();
    let key = format!("{ROOT}/keys/signing");
    sys.on(
        &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
        Ran::exit(
            255,
            "Load key: incorrect passphrase supplied to decrypt private key\n",
        ),
    );
    let held = format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -T {key}.pub");
    sys.on(&held, Ran::ok(""));
    sys.on(&held, Ran::exit(1, "Agent signature failed\n"));
    let mut e = env(&sys);
    e.agent_sock = Some("/tmp/agent.sock".into());
    assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Agent));
    e.tty = true;
    assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Terminal));
    e.tty = false;
    assert!(
        signing(&e, Path::new(&key))
            .unwrap_err()
            .contains("ssh-agent does not hold it")
    );
    e.agent_sock = None;
    e.tty = true;
    let before = sys.calls().len();
    assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Terminal));
    assert!(!sys.calls()[before..].iter().any(|c| c.contains("ssh-add")));
}

#[test]
fn the_principal_comes_from_allowed_signers_by_key_before_the_local_host_name() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    sys.on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("renamed-mac\n"),
    );
    let e = env(&sys);
    let inv = inventory_of(&sys);
    assert_eq!(principals(&inv, &e), ["signing@old-name"]);
    sys.file(&format!("{ROOT}/serial"), b"5\n");
    let mut hello = hello_of(&a);
    hello.floors = vec![crate::protocol::Floor {
        principal: "signing@old-name".into(),
        serial: 41,
    }];
    let floor = floor_of(&hello, &principals(&inv, &e));
    let b = body(&e, &a, floor, Action::Plan, Disrupt::WhenIdle).unwrap();
    assert_eq!(b.serial, 42);
    sys.file(
        &format!("{ROOT}/allowed_signers.previous"),
        format!("signing@older namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n")
            .as_bytes(),
    );
    assert_eq!(principals(&inv, &e), ["signing@old-name", "signing@older"]);
    let unlisted = Fake::root();
    unlisted.on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("renamed-mac\n"),
    );
    assert_eq!(
        principals(&Inventory::default(), &env(&unlisted)),
        ["signing@renamed-mac"]
    );
}

#[test]
fn an_upgrade_is_an_app_zip_only_when_the_bundle_verifies_as_aterms_team_here() {
    let exe = Path::new("/Applications/aterm (dev).app/Contents/MacOS/aterm");
    let archive = |bundle: bool| enroll::Archive {
        path: "/s/aterm-agent.zip".into(),
        sha256: "ab".repeat(32),
        exe: "aterm (dev).app/Contents/MacOS/aterm".into(),
        bundle,
    };
    let check = format!(
        "/usr/bin/codesign --verify --deep --strict -R {} '/Applications/aterm (dev).app'",
        crate::port::shell_word(APP_ZIP_REQUIREMENT)
    );
    let sys = Fake::root();
    sys.on(&check, Ran::ok(""));
    assert_eq!(upgrade_kind(&sys, exe, &archive(true)), "app-zip");
    let sys = Fake::root();
    sys.on(
        &check,
        Ran::exit(
            3,
            "test-requirement: code failed to satisfy specified code requirement(s)\n",
        ),
    );
    assert_eq!(upgrade_kind(&sys, exe, &archive(true)), "dev-zip");
    let sys = Fake::root();
    assert_eq!(
        upgrade_kind(&sys, Path::new("/w/aterm-machines-dev"), &archive(false)),
        "dev-zip"
    );
    assert!(sys.calls().is_empty(), "a bare binary is not checked");
}

#[test]
fn a_new_list_that_only_mentions_the_signing_key_in_a_comment_is_refused() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], false);
    sys.file(
        "/tmp/new_signers",
        format!(
            "# signing@old-name namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n\
             recovery@old-name namespaces=\"aterm-machines\" ssh-ed25519 {RECOVERY_BLOB}\n"
        )
        .as_bytes(),
    );
    let code = rekey(&env(&sys), &[], true, Path::new("/tmp/new_signers"), None);
    assert_eq!(code, ExitCode::from(2));
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("/usr/bin/ssh ")),
        "no host asked"
    );
}

#[test]
fn the_recovery_keys_public_half_is_found_beside_it_or_kept_on_the_controller() {
    let sys = Fake::root();
    let e = env(&sys);
    sys.file(
        &format!("{ROOT}/keys/recovery.pub"),
        b"ssh-ed25519 AAAA recovery@ctl\n",
    );
    assert_eq!(
        public_of(&e, Path::new("/Volumes/KEY/recovery")).unwrap(),
        PathBuf::from(format!("{ROOT}/keys/recovery.pub"))
    );
    sys.file(
        "/Volumes/KEY/recovery.pub",
        b"ssh-ed25519 AAAA recovery@ctl\n",
    );
    assert_eq!(
        public_of(&e, Path::new("/Volumes/KEY/recovery")).unwrap(),
        PathBuf::from("/Volumes/KEY/recovery.pub")
    );
    let err = public_of(&e, Path::new("/Volumes/KEY/other")).unwrap_err();
    assert!(
        err.contains("/Volumes/KEY/other.pub") && err.contains("keys/other.pub"),
        "{err}"
    );
}

#[test]
fn the_controllers_list_is_replaced_keeping_the_old_one_and_hosts_behind_are_known() {
    let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
    let sys = controller(&[&a, &b], false);
    let e = env(&sys);
    let old = sys.written(&format!("{ROOT}/allowed_signers")).unwrap();
    let new = format!("signing@new namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n");
    record_signers(&e, &a, &text_sha256(&new));
    replace_signers(&e, &new).unwrap();
    assert_eq!(
        sys.written(&format!("{ROOT}/allowed_signers.previous"))
            .unwrap(),
        old
    );
    assert_eq!(
        sys.written(&format!("{ROOT}/allowed_signers")).unwrap(),
        new.as_bytes()
    );
    assert_eq!(state_of(&sys, &a).signers_sha256, Some(text_sha256(&new)));
    assert_eq!(state_of(&sys, &b).signers_sha256, None);
    replace_signers(&e, &new).unwrap();
    assert_eq!(
        sys.written(&format!("{ROOT}/allowed_signers.previous"))
            .unwrap(),
        old
    );
    assert_eq!(
        principals(&inventory_of(&sys), &e),
        ["signing@new", "signing@old-name"]
    );
}

const ALICE_OLD: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";
const ALICE_NEW: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
const MACHINE_TOKEN: &str = "fixture_machine_user_token_only";
const KEYS_PAGE: &str = "/user/keys?per_page=100&page=1";

fn rest(status: u16, body: &str) -> Result<crate::port::GithubResponse, String> {
    Ok(crate::port::GithubResponse {
        status,
        headers: Vec::new(),
        body: body.as_bytes().to_vec(),
    })
}

fn with_token(sys: &Fake) {
    super::super::github_keys_tests::holds_token(sys, &paths(), MACHINE_TOKEN);
    sys.on_github_rest(
        crate::port::RestMethod::Get,
        "/user",
        rest(200, r#"{"login":"fleetbot"}"#),
    );
}

fn lists(sys: &Fake, held: &[(u64, &str)]) {
    let keys: Vec<_> = held
        .iter()
        .map(|(id, body)| aterm_json::json!({"id": id, "key": format!("ssh-ed25519 {body}")}))
        .collect();
    sys.on_github_rest(
        crate::port::RestMethod::Get,
        KEYS_PAGE,
        rest(200, &aterm_json::to_string(&keys).unwrap()),
    );
}

fn posts(sys: &Fake, id: u64) {
    sys.on_github_rest(
        crate::port::RestMethod::Post,
        "/user/keys",
        rest(201, &format!(r#"{{"id":{id}}}"#)),
    );
}

fn deletes(sys: &Fake, id: u64) {
    sys.on_github_rest(
        crate::port::RestMethod::Delete,
        &format!("/user/keys/{id}"),
        rest(204, ""),
    );
}

fn post_key(body: &str) -> String {
    format!(
        r#"github POST /user/keys {{"title":"aterm-machines:lab-01:alice","key":"ssh-ed25519 {body}"}}"#
    )
}

fn key_row(body: &str) -> ItemReport {
    let mut r = row("github.key@alice", "github", Outcome::Ok);
    r.detail = Some(format!("ssh-ed25519 {body} aterm-machines:lab-01:alice"));
    r
}

#[test]
fn a_users_new_key_is_registered_and_the_one_it_replaced_deleted() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], true);
    with_token(&sys);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    let state = HostState {
        github_keys: vec![("alice".into(), 5)],
        github_key_text: vec![(5, format!("ssh-ed25519 {ALICE_OLD}"))],
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Apply, vec![key_row(ALICE_OLD)]),
        Source::Baseline,
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.starts_with("github ") || c.contains("credential fill")),
        "{:?}",
        sys.calls()
    );
    lists(&sys, &[(5, ALICE_OLD)]);
    posts(&sys, 9);
    lists(&sys, &[(5, ALICE_OLD), (9, ALICE_NEW)]);
    lists(&sys, &[(9, ALICE_NEW)]);
    deletes(&sys, 5);
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Apply, vec![key_row(ALICE_NEW)]),
        Source::Baseline,
    );
    let state = state_of(&sys, &a);
    assert_eq!(state.github_keys, [("alice".to_owned(), 9)]);
    assert_eq!(
        state.github_key_text,
        [(9, format!("ssh-ed25519 {ALICE_NEW}"))]
    );
    let calls = sys.calls();
    assert!(calls.contains(&post_key(ALICE_NEW)), "{calls:?}");
    assert!(calls.contains(&"github DELETE /user/keys/5".to_owned()));
    assert!(
        calls
            .iter()
            .all(|c| !c.contains(MACHINE_TOKEN) && !c.contains("/gh "))
    );
}

#[test]
fn a_key_github_created_is_recorded_when_another_key_fails() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], true);
    with_token(&sys);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    lists(&sys, &[]);
    posts(&sys, 9);
    sys.on_github_rest(
        crate::port::RestMethod::Post,
        "/user/keys",
        rest(
            422,
            r#"{"message":"Validation Failed","errors":[{"message":"key is already in use"}]}"#,
        ),
    );
    let mut bob = row("github.key@bob", "github", Outcome::Ok);
    bob.detail = Some(format!("ssh-ed25519 {ALICE_NEW} aterm-machines:lab-01:bob"));
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Apply, vec![key_row(ALICE_OLD), bob]),
        Source::Baseline,
    );
    let state = state_of(&sys, &a);
    assert_eq!(state.github_keys, [("alice".to_owned(), 9)]);
    assert_eq!(
        state.github_key_text,
        [(9, format!("ssh-ed25519 {ALICE_OLD}"))]
    );
    let posted = sys
        .calls()
        .iter()
        .filter(|c| c.starts_with("github POST /user/keys"))
        .count();
    assert_eq!(posted, 2);
}

#[test]
fn a_revoked_key_is_never_registered_again_and_a_plan_registers_nothing() {
    let a = lab(1, UUID_1);
    let sys = controller(&[&a], true);
    with_token(&sys);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    let state = HostState {
        github_keys: vec![("alice".into(), 5)],
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
    lists(&sys, &[(5, ALICE_OLD)]);
    lists(&sys, &[]);
    deletes(&sys, 5);
    assert_eq!(github_revoke(&e, "lab-01"), ExitCode::SUCCESS);
    let state = state_of(&sys, &a);
    assert!(state.github_keys.is_empty());
    assert_eq!(state.github_revoked, [format!("ssh-ed25519 {ALICE_OLD}")]);
    let calls = sys.calls().len();
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Apply, vec![key_row(ALICE_OLD)]),
        Source::Baseline,
    );
    assert!(!sys.calls()[calls..].iter().any(|c| c.contains("POST")));
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Plan, vec![key_row(ALICE_NEW)]),
        Source::Baseline,
    );
    assert!(!sys.calls()[calls..].iter().any(|c| c.contains("POST")));
    lists(&sys, &[]);
    posts(&sys, 11);
    after_run(
        &e,
        &inv,
        &a,
        report(Mode::Apply, vec![key_row(ALICE_NEW)]),
        Source::Baseline,
    );
    assert_eq!(state_of(&sys, &a).github_keys, [("alice".to_owned(), 11)]);
}

/// A controller of `hosts` whose `[github]` row sends the key calls over agh, inline, to
/// `fake` on loopback (plan §17).
fn over_agh(hosts: &[&Host], fake: &agh_fake::FakeGithub) -> Fake {
    let (sys, _token) =
        super::super::github_keys_tests::inline_on(controller(hosts, false), &paths(), fake);
    names(
        &sys,
        hosts,
        &format!(
            "[github]\naccount = \"{}\"\nkeys = \"agh-inline\"\n",
            super::super::github_keys_tests::LOGIN
        ),
    );
    sys
}

/// `METHOD path` of each request `fake` saw.
fn agh_saw(fake: &agh_fake::FakeGithub) -> Vec<String> {
    super::super::github_keys_tests::requests(&fake.seen())
}

#[test]
fn a_runs_hosts_share_one_agh_client_so_their_key_writes_are_paced() {
    let fake = agh_fake::FakeGithub::start();
    let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
    let sys = over_agh(&[&a, &b], &fake);
    let e = env(&sys);
    let inv = inventory_of(&sys);
    // `run`'s loop: every host's report through the one `KeyCalls` it opens.
    let key_calls = super::super::github::KeyCalls::new(&sys, &paths());
    for (host, body) in [(&a, ALICE_OLD), (&b, ALICE_NEW)] {
        after_run_with(
            &e,
            &inv,
            host,
            report(Mode::Apply, vec![key_row(body)]),
            Source::Baseline,
            &key_calls,
        );
    }
    // The second host's POST waits out agh's second after the first's, and the inline
    // token's login is bound once.
    assert_eq!(System::github_fx(&sys).slept(), [Duration::from_secs(1)]);
    let saw = agh_saw(&fake);
    assert_eq!(
        saw.iter().filter(|r| *r == "GET /user").count(),
        1,
        "{saw:?}"
    );
    assert_eq!(
        saw.iter().filter(|r| *r == "POST /user/keys").count(),
        2,
        "{saw:?}"
    );
    let held = fake.keys(super::super::github_keys_tests::LOGIN);
    for (host, body) in [(&a, ALICE_OLD), (&b, ALICE_NEW)] {
        let state = state_of(&sys, host);
        let [(user, id)] = state.github_keys.as_slice() else {
            panic!("{}: {:?}", host.name, state.github_keys);
        };
        assert_eq!(user, "alice");
        assert!(
            held.iter()
                .any(|k| k.id == *id && k.key == format!("ssh-ed25519 {body}")),
            "{held:?}"
        );
    }
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("github ")),
        "no REST call: {:?}",
        sys.calls()
    );
}

#[test]
fn github_revoke_deletes_over_agh_when_the_row_says() {
    let fake = agh_fake::FakeGithub::start();
    let a = lab(1, UUID_1);
    let sys = over_agh(&[&a], &fake);
    let id = fake.add_key(
        super::super::github_keys_tests::LOGIN,
        "aterm-machines:lab-01:alice",
        &format!("ssh-ed25519 {ALICE_OLD}"),
    );
    let state = HostState {
        github_keys: vec![("alice".into(), id)],
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
    assert_eq!(github_revoke(&env(&sys), "lab-01"), ExitCode::SUCCESS);
    let saw = agh_saw(&fake);
    assert!(saw.contains(&format!("DELETE /user/keys/{id}")), "{saw:?}");
    assert!(fake.keys(super::super::github_keys_tests::LOGIN).is_empty());
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("github ")),
        "no REST call: {:?}",
        sys.calls()
    );
    let state = state_of(&sys, &a);
    assert!(state.github_keys.is_empty());
    assert_eq!(state.github_revoked, [format!("ssh-ed25519 {ALICE_OLD}")]);
}

#[test]
fn init_keeps_the_key_and_refuses_an_unconfirmed_passphrase() {
    for (label, after, protected) in [
        (
            "protected",
            Ran::exit(
                255,
                "Load key: incorrect passphrase supplied to decrypt private key\n",
            ),
            true,
        ),
        ("empty", Ran::ok("ssh-ed25519 public\n"), false),
        ("unreadable", Ran::exit(1, "permission denied\n"), false),
        (
            "interrupted",
            Ran {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
            false,
        ),
    ] {
        let sys = uninitialized(false);
        assert_eq!(init(&env(&sys), false, &mut Vec::new()), ExitCode::SUCCESS);
        let key = format!("{ROOT}/keys/signing");
        let private = b"the same private-key identity";
        sys.file(&key, private);
        let query = format!("/usr/bin/ssh-keygen -y -P '' -f {key}");
        sys.on(&query, Ran::ok("ssh-ed25519 public\n"));
        sys.on(&query, after);
        let protect = format!("tty /usr/bin/ssh-keygen -p -f {key}");
        sys.on(&protect, Ran::ok(""));
        let mut e = env(&sys);
        e.tty = true;
        let before = sys.calls().len();
        let mut out = Vec::new();
        assert_eq!(
            init(&e, true, &mut out),
            if protected {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            },
            "INIT_PASSPHRASE_NEEDS_CONFIRMED_PROTECTION: {label}"
        );
        assert_eq!(sys.read_file(Path::new(&key)).unwrap(), private, "{label}");
        let calls = sys.calls()[before..].to_vec();
        assert!(calls.contains(&protect), "the actual change ran: {calls:?}");
        assert!(!calls.iter().any(|c| c.contains("-t ed25519")), "{calls:?}");
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            out.contains("gave the signing key a passphrase"),
            protected,
            "{label}: {out}"
        );
    }
}

#[test]
fn a_new_controller_is_set_up_by_the_first_verb_that_needs_it_and_an_old_one_left() {
    let sys = uninitialized(false);
    set_up_if_new(&sys, &paths());
    assert!(sys.written(&format!("{ROOT}/machines.toml")).is_some());
    assert!(sys.written(&format!("{ROOT}/allowed_signers")).is_some());
    assert!(
        sys.calls()
            .iter()
            .any(|c| c.contains("ssh-keygen -q -t ed25519 -N ''")),
        "{:?}",
        sys.calls()
    );
    let sys = uninitialized(false);
    sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
    set_up_if_new(&sys, &paths());
    assert!(sys.written(&format!("{ROOT}/allowed_signers")).is_none());
    assert!(!sys.calls().iter().any(|c| c.contains("ssh-keygen")));
}

#[test]
fn a_starter_baseline_nobody_edited_follows_the_starter_shipped_now() {
    const HOMEBREW_AND_APPS: &str = "[homebrew]\n\
         pkg = { url = \"https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew.pkg\", \
         sha256 = \"e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15\", \
         team_id = \"927JGANW46\" }\n\
         formulae = [\"git\", \"ripgrep\", \"fd\", \"jq\", \"tmux\", \"fzf\", \"wget\", \
         \"htop\", \"coreutils\", \"git-lfs\"]\ncasks = []\n\n\
         [[app]]\nname = \"Emacs\"\nkind = \"dmg-app\"\n\
         url = \"https://emacsformacosx.com/emacs-builds/Emacs-31.1-universal.dmg\"\n\
         sha256 = \"f0383bcbf0104947d3a46266aae43e3dc9e38fd68eeb35f11ee6b9444277b8ec\"\n\
         team_id = \"5BRAQAFB8B\"\nbundle = \"Emacs.app\"\nversion = \"31.1\"\n\n\
         [[app]]\nname = \"Caffeine\"\nkind = \"dmg-app\"\n\
         url = \"https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg\"\n\
         sha256 = \"1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723\"\n\
         team_id = \"YD6LEYT6WZ\"\nbundle = \"Caffeine.app\"\nversion = \"1.1.4\"\n\n";
    const GH_ROW: &str = "# gh's installer package carries no installer signature; its binary is checked against\n\
         # GitHub's Team ID instead (design §8).\n[[app]]\nname = \"gh\"\nkind = \"pkg\"\n\
         url = \"https://github.com/cli/cli/releases/download/v2.102.0/gh_2.102.0_macOS_universal.pkg\"\n\
         sha256 = \"1787e65f36626245a6b3cee353a458f6ea4372008548ceb8fb5d5dfb55b006d0\"\n\
         team_id = \"VEKTX9H2N7\"\nunsigned = true\npkg_id = \"com.github.cli\"\n\
         binary = \"/usr/local/bin/gh\"\nversion = \"2.102.0\"\n\n";
    let pointer = "# Other software is yours to add: a [homebrew] table installs Homebrew and its \
         formulae,\n# an [[app]] row one signed app (docs/DESIGN-machines-2026-10-16.md §8). For \
         keeping a Mac\n# awake and editing a file, aterm has its own `aterm acaffeine` and \
         `aterm edit`.\n\n";
    assert!(templates::DEFAULT_BASELINE.contains(pointer));
    let before_rename =
        templates::DEFAULT_BASELINE.replace("`aterm acaffeine`", "`aterm acaffiene`");
    assert!(templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&before_rename).as_str()));
    let before_vault_daemon = before_rename.replace("vault_server = true\n", "");
    assert!(
        templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&before_vault_daemon).as_str())
    );
    let previous = before_vault_daemon.replace(
        &pointer.replace("acaffeine", "acaffiene"),
        HOMEBREW_AND_APPS,
    );
    assert!(
        templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&previous).as_str()),
        "the digest is of the starter as a6c1342e82 wrote it"
    );
    let anchor = "# A per-host GitHub key for each user";
    let earlier = previous.replace(anchor, &format!("{GH_ROW}{anchor}"));
    assert!(
        templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&earlier).as_str()),
        "the digest is of the starter as 326cdca677 wrote it"
    );
    let older = earlier.replace(
        "# host_name \"\" unsets a HostName set by hand, so `hostname`, shell prompts and \
         ssh show the\n# new Local hostname rather than the old name; a dotted name sets one \
         instead.\n",
        "",
    );
    let older = older.replace("host_name = \"\"\n", "");
    assert!(
        templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&older).as_str()),
        "the digest is of the starter as 6045a51374 wrote it"
    );
    assert!(
        !templates::EARLIER_DEFAULT_BASELINES
            .contains(&text_sha256(templates::DEFAULT_BASELINE).as_str()),
        "the starter shipped now is not an earlier one"
    );
    let default = format!("{ROOT}/baselines/default.toml");
    for (on_disk, after) in [
        (
            before_rename.clone(),
            templates::DEFAULT_BASELINE.to_owned(),
        ),
        (
            before_vault_daemon.clone(),
            templates::DEFAULT_BASELINE.to_owned(),
        ),
        (
            format!("{before_rename}# mine\n"),
            format!("{before_rename}# mine\n"),
        ),
        (
            format!("{before_vault_daemon}# mine\n"),
            format!("{before_vault_daemon}# mine\n"),
        ),
        (previous.clone(), templates::DEFAULT_BASELINE.to_owned()),
        (earlier.clone(), templates::DEFAULT_BASELINE.to_owned()),
        (format!("{previous}# mine\n"), format!("{previous}# mine\n")),
        (format!("{earlier}# mine\n"), format!("{earlier}# mine\n")),
        (
            templates::DEFAULT_BASELINE.to_owned(),
            templates::DEFAULT_BASELINE.to_owned(),
        ),
    ] {
        let sys = uninitialized(false);
        sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
        sys.file(&default, on_disk.as_bytes());
        set_up_if_new(&sys, &paths());
        assert_eq!(
            String::from_utf8(sys.written(&default).unwrap()).unwrap(),
            after
        );
    }
    let sys = uninitialized(false);
    sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
    set_up_if_new(&sys, &paths());
    assert!(sys.written(&default).is_none());
}

fn timed_out(host: &Host) -> Ran {
    Ran::exit(
        255,
        &format!(
            "ssh: connect to host {} port 22: Operation timed out\r\n",
            host.address
        ),
    )
}

fn dhcp_vm() -> (Fake, Host, Host) {
    let vm = lab(9, UUID_1);
    let sys = controller(&[&vm], false);
    let mut said = hello_of(&vm);
    said.local_host_name = "lab-09".into();
    let state = HostState {
        hello: Some(aterm_json::to_string(&said).unwrap()),
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &vm.uuid, &state).unwrap();
    let named = Host {
        address: "lab-09.local".into(),
        ..vm.clone()
    };
    (sys, vm, named)
}

#[test]
fn a_mac_whose_dhcp_address_moved_is_followed_to_its_local_hostname_once_its_key_answers() {
    let (sys, vm, named) = dhcp_vm();
    sys.on(&request(&vm, "hello"), timed_out(&vm)).on(
        &request(&named, "hello"),
        Ran::ok(&aterm_json::to_string(&hello_of(&vm)).unwrap()),
    );
    let reached = reach_one(&env(&sys), &vm);
    assert!(reached.hello.is_ok());
    assert_eq!(reached.host.address, named.address);
    assert_eq!(inventory_of(&sys).hosts[0].address, named.address);
    assert_eq!(found_lead(&vm), format!("{:<16} moved: ", vm.name));
    let asked = request(&named, "hello");
    assert!(sys.calls().contains(&asked));
    assert!(asked.starts_with("/usr/bin/ssh "), "{asked}");
    assert!(asked.contains(&format!("HostKeyAlias={UUID_1}")), "{asked}");
    assert!(asked.contains("StrictHostKeyChecking=yes"), "{asked}");
    let state = state_of(&sys, &vm);
    assert_eq!((state.last_seen, state.unreachable_since), (sys.now, None));
    let (sys, vm, named) = dhcp_vm();
    sys.on(&request(&vm, "hello"), Ran::exit(255, HOST_KEY_CHANGED))
        .on(
            &request(&named, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&vm)).unwrap()),
        );
    assert!(reach_one(&env(&sys), &vm).hello.is_ok());
    assert_eq!(inventory_of(&sys).hosts[0].address, named.address);
}

#[test]
fn another_mac_at_its_local_hostname_moves_nothing() {
    let (sys, vm, named) = dhcp_vm();
    sys.on(&request(&vm, "hello"), timed_out(&vm))
        .on(&request(&named, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
    let reached = reach_one(&env(&sys), &vm);
    assert!(matches!(
        reached.hello,
        Err(HelloError::Ssh(ssh::Failure::TimedOut))
    ));
    assert_eq!(reached.host.address, vm.address);
    assert_eq!(inventory_of(&sys).hosts[0].address, vm.address);
    let state = state_of(&sys, &vm);
    assert_eq!(state.unreachable_since, Some(sys.now));
    assert_eq!(state.contact_error, None);
    let (sys, vm, named) = dhcp_vm();
    let mut other = hello_of(&vm);
    other.uuid = UUID_2.into();
    sys.on(&request(&vm, "hello"), timed_out(&vm)).on(
        &request(&named, "hello"),
        Ran::ok(&aterm_json::to_string(&other).unwrap()),
    );
    assert!(reach_one(&env(&sys), &vm).hello.is_err());
    assert_eq!(inventory_of(&sys).hosts[0].address, vm.address);
}

#[test]
fn only_an_ip_address_that_does_not_answer_is_looked_for_by_name() {
    let (sys, vm, named) = dhcp_vm();
    sys.on(
        &request(&vm, "hello"),
        Ran::ok(&aterm_json::to_string(&hello_of(&vm)).unwrap()),
    );
    assert!(reach_one(&env(&sys), &vm).hello.is_ok());
    assert!(!sys.calls().contains(&request(&named, "hello")));
    let (sys, vm, named) = dhcp_vm();
    sys.on(
        &request(&vm, "hello"),
        Ran::exit(
            255,
            "admin@192.168.64.9: Permission denied (publickey).\r\n",
        ),
    );
    assert!(reach_one(&env(&sys), &vm).hello.is_err());
    assert!(!sys.calls().contains(&request(&named, "hello")));
    let vm = lab(9, UUID_1);
    let sys = controller(&[&vm], false);
    sys.on(&request(&vm, "hello"), timed_out(&vm));
    assert!(reach_one(&env(&sys), &vm).hello.is_err());
    let ssh_calls = |sys: &Fake| {
        sys.calls()
            .iter()
            .filter(|c| c.starts_with("/usr/bin/ssh "))
            .count()
    };
    assert_eq!(ssh_calls(&sys), 1, "{:?}", sys.calls());
    let state = HostState {
        hello: Some(aterm_json::to_string(&hello_of(&vm)).unwrap()),
        ..HostState::default()
    };
    inventory::save_state(&sys, &paths(), &vm.uuid, &state).unwrap();
    assert!(reach_one(&env(&sys), &vm).hello.is_err());
    assert_eq!(ssh_calls(&sys), 2, "{:?}", sys.calls());
    assert_eq!(inventory_of(&sys).hosts[0].address, vm.address);
}

#[test]
fn the_verbs_that_read_state_send_nothing() {
    let (sys, vm, _) = dhcp_vm();
    let e = env(&sys);
    let _ = ls(&e);
    let _ = status(&e, &[], true, false);
    let _ = outstanding(&e);
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("/usr/bin/ssh ")),
        "{:?}",
        sys.calls()
    );
    let inv = inventory_of(&sys);
    assert_eq!(inv.hosts.len(), 1);
    assert_eq!(inv.hosts[0].address, vm.address);
}

#[test]
fn session_extent_requires_a_complete_observation_before_claiming_an_empty_host() {
    use crate::protocol::SessionSnapshot;
    assert_eq!(session_extent(None), SessionExtent::Unobserved);
    let mut inventory = SessionSnapshot::default();
    assert_eq!(session_extent(Some(&inventory)), SessionExtent::Unobserved);
    inventory.rows.push(crate::protocol::HostedSession {
        instance_pid: 42,
        local_id: 1,
        sid: "s-observed".into(),
        ..crate::protocol::HostedSession::default()
    });
    assert_eq!(session_extent(Some(&inventory)), SessionExtent::Partial);
    inventory.complete = true;
    assert_eq!(session_extent(Some(&inventory)), SessionExtent::Complete);
    inventory.rows.clear();
    assert_eq!(session_extent(Some(&inventory)), SessionExtent::Complete);
}

#[test]
fn hello_carries_partial_session_observations_without_promoting_missing_metadata() {
    let host = lab(1, UUID_1);
    let mut hello = hello_of(&host);
    hello.sessions.rows.push(crate::protocol::HostedSession {
        instance_pid: 42,
        local_id: 1,
        sid: "s-observed".into(),
        ..crate::protocol::HostedSession::default()
    });
    hello.sessions.complete = false;
    let mut value = aterm_json::to_value(&hello).unwrap();
    let observed: Hello = aterm_json::from_value(value.clone()).unwrap();
    assert_eq!(observed.sessions, hello.sessions);
    assert!(!observed.sessions.complete);
    value.as_object_mut().unwrap().remove("sessions_complete");
    let missing: Hello = aterm_json::from_value(value).unwrap();
    assert_eq!(missing.sessions.rows, hello.sessions.rows);
    assert!(!missing.sessions.complete);
    assert_eq!(
        session_extent(Some(&missing.sessions)),
        SessionExtent::Partial
    );
}

#[test]
fn tabs_sweeps_live_hosts_and_prints_sessions_and_ai_cli_status() {
    let host = lab(1, UUID_1);
    let sys = controller(&[&host], false);
    let mut h = hello_of(&host);
    h.sessions.complete = true;
    h.sessions.rows.push(crate::protocol::HostedSession {
        instance_pid: 9001,
        local_id: 2,
        sid: "s-tab2".into(),
        state: "alive".into(),
        title: "Ship Fleet".into(),
        window: "0".into(),
        active: true,
        focused: true,
        program: Some("claude".into()),
        agent: Some("working".into()),
        agent_detail: Some("thinking".into()),
        agent_since_ms: Some(12_000),
        program_state: None,
        human_ms: Some(45_000),
        autopilot: Some("cruise".into()),
        role: Some("operator".into()),
        attention: None,
        supervisor: Some("aterm".into()),
        task: Some("finish lan".into()),
        cols: Some(211),
        rows: Some(70),
        cwd: Some("~/aterm".into()),
    });
    let hello_json = format!("{}\n", aterm_json::to_string(&h).unwrap());
    sys.on(&request(&host, "hello"), Ran::ok(&hello_json));

    let e = env(&sys);
    assert_eq!(tabs(&e, &[], false), ExitCode::SUCCESS);
    let saved = state_of(&sys, &host);
    let parsed: Hello = aterm_json::from_str(saved.hello.as_deref().unwrap()).unwrap();
    assert!(parsed.sessions.complete);
    assert_eq!(parsed.sessions.rows.len(), 1);
    assert_eq!(parsed.sessions.rows[0].program.as_deref(), Some("claude"));
    assert_eq!(
        format_session_ai_status(&parsed.sessions.rows[0]),
        "working: thinking (12s)"
    );
    assert_eq!(working_ai_sessions(&parsed.sessions.rows), 1);
    let mut quiet = parsed.sessions.rows[0].clone();
    for word in ["waiting", "unknown", "idle"] {
        quiet.agent = Some(word.into());
        assert_eq!(working_ai_sessions(std::slice::from_ref(&quiet)), 0);
    }
    quiet.agent = Some("busy".into());
    assert_eq!(working_ai_sessions(std::slice::from_ref(&quiet)), 1);
    assert_eq!(describe_host_hardware(None), "hardware unknown");
    assert_eq!(describe_host_hardware(Some("Mac17,4")), "Mac17,4");
    assert_eq!(
        format_os_and_ssh(None, None, None, None),
        "OS version unknown · SSH banner unknown"
    );
    assert_eq!(
        format_os_and_ssh(None, None, Some(u32::MAX), None),
        format!("Darwin {} · SSH banner unknown", u32::MAX)
    );
    assert_eq!(
        format_os_and_ssh(
            Some("27.0.1"),
            Some("26A1"),
            None,
            Some("OpenSSH_measured detail")
        ),
        "macOS 27.0.1 (26A1) · OpenSSH_measured"
    );
    let banner = |bytes: &[u8], expired| {
        use std::io::Write;
        use std::net::{Shutdown, TcpListener, TcpStream};
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        peer.write_all(bytes).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let now = std::time::Instant::now();
        let deadline = if expired {
            now - Duration::from_secs(1)
        } else {
            now + Duration::from_secs(60)
        };
        read_ssh_banner(&client, deadline)
    };
    assert_eq!(
        banner(b"SSH-2.0-observed_peer\r\n", false).as_deref(),
        Some("observed_peer")
    );
    let mut oversized = b"SSH-2.0-".to_vec();
    oversized.extend([b'x'; 255]);
    oversized.push(b'\n');
    assert_eq!(banner(&oversized, false), None);
    assert_eq!(banner(b"SSH-2.0-partial", false), None);
    assert_eq!(banner(b"SSH-2.0-\xff\r\n", false), None);
    assert_eq!(banner(b"SSH-2.0-observed_peer\r\n", true), None);

    let summary = HostVitals {
        gpu_name: Some("NVIDIA GB10".into()),
        rtt_us: Some(2_100),
        ..Default::default()
    }
    .format_summary_line();
    assert!(summary.contains("NVIDIA GB10"), "{summary}");
    assert!(summary.contains("RTT 2.1 ms"), "{summary}");

    assert_eq!(
        monitor(&e, &[], false, false, true, false, 2),
        ExitCode::SUCCESS
    );
}

/// `here apply --plain`, `aterm init`'s root half for the Command Line Tools, prints no
/// report and no line of its own and reads no baseline file (it applies the tools' own): it
/// ends on 0 once that baseline is done, else on the code of why it did not finish
/// ([`crate::init_clt::Unfinished::code`]), which `aterm init` says in plain words.
#[test]
fn here_apply_plain_says_nothing_and_exits_with_why_it_did_not_finish() {
    use crate::init_clt::Unfinished;
    let baseline = "/b/command-line-tools.toml";
    let mac = || {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/mdutil -s /System/Volumes/Data",
            Ran::ok("Indexing disabled."),
        );
        sys.born(std::process::id(), 123).running(&[]);
        sys.stat_of(
            "/dev/console",
            crate::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys
    };
    let none_selected = || {
        Ran::exit(
            2,
            "xcode-select: error: Unable to get active developer directory.",
        )
    };
    let catalog = "/usr/sbin/softwareupdate -l";
    let plain = |sys: &Fake| {
        here_report(
            sys,
            Mode::Apply,
            HereBaseline::BuiltIn(crate::init_clt::RootPlan::WithTools),
            Plain::default(),
            HereOutput::Plain,
        )
    };
    let class = |why: Unfinished| Some(ExitCode::from(why.code().unwrap()));

    let with_tools = |sys: &Fake| {
        for executable in [
            crate::items::developer::CLT_GIT,
            crate::items::developer::CLT_CLANG,
        ] {
            sys.stat_of(
                executable,
                crate::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o755,
                    dir: false,
                    symlink: false,
                    len: 1,
                },
            );
        }
        sys.on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Library/Developer/CommandLineTools\n"),
        );
    };
    // Installed and selected already: done, and nothing to print.
    let done = mac();
    with_tools(&done);
    assert!(matches!(plain(&done), Ok(None)));

    // Independent work must not install or select CLT, even with no tools present.
    let independent = mac();
    assert!(matches!(
        here_report(
            &independent,
            Mode::Apply,
            HereBaseline::BuiltIn(crate::init_clt::RootPlan::StepsOnly),
            Plain::default(),
            HereOutput::Plain
        ),
        Ok(None)
    ));
    assert!(
        independent
            .written(crate::items::developer::PLACEHOLDER)
            .is_none()
    );
    assert!(
        !independent
            .calls()
            .iter()
            .any(|call| call.contains("softwareupdate") || call.contains("xcode-select")),
        "{:?}",
        independent.calls()
    );

    let indexing = Fake::root();
    indexing.born(std::process::id(), 123).running(&[]);
    indexing.stat_of(
        "/dev/console",
        crate::port::Stat {
            uid: 0,
            gid: 0,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    indexing
        .on(
            "/usr/bin/mdutil -s /System/Volumes/Data",
            Ran::ok("Indexing enabled."),
        )
        .on(
            "/usr/bin/mdutil -s /System/Volumes/Data",
            Ran::ok("Indexing disabled."),
        )
        .on("/usr/bin/mdutil -i off /System/Volumes/Data", Ran::ok(""));
    assert!(matches!(
        here_report(
            &indexing,
            Mode::Apply,
            HereBaseline::BuiltIn(crate::init_clt::RootPlan::StepsOnly),
            Plain::default(),
            HereOutput::Plain,
        ),
        Ok(None)
    ));
    assert!(
        indexing
            .calls()
            .iter()
            .any(|call| call == "/usr/bin/mdutil -i off /System/Volumes/Data")
    );
    indexing.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("Indexing enabled."),
    );
    assert_eq!(crate::init_spotlight::indexing(&indexing), Ok(false));
    assert!(
        here_report(
            &indexing,
            Mode::Apply,
            HereBaseline::BuiltIn(crate::init_clt::RootPlan::StepsOnly),
            Plain::default(),
            HereOutput::Plain,
        )
        .is_err(),
        "a successful mdutil exit cannot hide indexing that stayed enabled"
    );

    // Explicit naming authorization is a different, fixed root scope. No tools,
    // LAN, host-name or private-preference inventory is needed to install it.
    for permit_acl in [true, false] {
        let permission = Fake::root();
        permission.stat_of(
            "/dev/console",
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        permission
            .on("/usr/bin/dscl . -list /Users UniqueID", Ran::ok("alice 501\n"))
            .on("/usr/bin/dscl . -read /Groups/admin GroupMembership", Ran::ok("GroupMembership: alice\n"))
            .on("/usr/bin/dscl . -read /Users//alice UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
                Ran::ok("UniqueID: 501\nPrimaryGroupID: 20\nNFSHomeDirectory: /Users//alice\nUserShell: /bin/zsh\n"));
        permission.simulate(move |_, command| {
            if command.program == Path::new("/bin/chmod") {
                return Some(if permit_acl {
                    Ran::ok("")
                } else {
                    Ran::exit(1, "ACL refused")
                });
            }
            (command.program == Path::new(crate::root_steps::VISUDO)).then(|| Ran::ok(""))
        });
        let authorize = || {
            here_report(
                &permission,
                Mode::Apply,
                HereBaseline::BuiltIn(crate::init_clt::RootPlan::NameRuleOnly),
                Plain::default(),
                HereOutput::Plain,
            )
        };
        if permit_acl {
            assert!(matches!(authorize(), Ok(None)));
            let bytes = permission
                .read_file(Path::new(crate::root_steps::ADMITTED_NAME_RULE))
                .unwrap();
            // Same-text opaque rules must still receive the read ACL on explicit repair.
            assert!(matches!(authorize(), Ok(None)));
            assert_eq!(
                permission
                    .read_file(Path::new(crate::root_steps::ADMITTED_NAME_RULE))
                    .unwrap(),
                bytes
            );
            assert_eq!(
                permission
                    .calls()
                    .iter()
                    .filter(|call| call.starts_with("/bin/chmod +a "))
                    .count(),
                2
            );
            assert_eq!(
                permission.mode_of(crate::root_steps::ADMITTED_NAME_RULE),
                Some(0o440)
            );
        } else {
            assert!(authorize().err() == class(Unfinished::Other));
            assert!(
                permission
                    .written(crate::root_steps::ADMITTED_NAME_RULE)
                    .is_none()
            );
        }
        assert!(
            permission
                .calls()
                .iter()
                .filter(|call| call.starts_with('/'))
                .all(|call| call.starts_with("/usr/bin/dscl ")
                    || call.starts_with("/bin/chmod +a ")
                    || call.starts_with(crate::root_steps::VISUDO)),
            "{:?}",
            permission.calls()
        );
        if permit_acl {
            // A naming repair remains in the same compiled root transaction
            // when setup also owes another root step. Matching bytes alone do
            // not prove the ordinary account can read the rule's current ACL.
            let before = permission
                .calls()
                .iter()
                .filter(|call| call.starts_with("/bin/chmod +a "))
                .count();
            permission.on(
                "/usr/bin/mdutil -s /System/Volumes/Data",
                Ran::ok("Indexing disabled."),
            );
            assert!(matches!(
                here_report(
                    &permission,
                    Mode::Apply,
                    HereBaseline::BuiltIn(crate::init_clt::RootPlan::StepsOnly),
                    Plain::default(),
                    HereOutput::Plain,
                ),
                Ok(None)
            ));
            assert_eq!(
                permission
                    .calls()
                    .iter()
                    .filter(|call| call.starts_with("/bin/chmod +a "))
                    .count(),
                before + 1,
                "the existing batch must refresh its one naming permission"
            );
            assert!(
                !permission
                    .calls()
                    .iter()
                    .any(|call| call.contains("scutil --set") || call.contains("softwareupdate"))
            );
        }
    }
    let no_console = mac();
    assert!(
        here_report(
            &no_console,
            Mode::Apply,
            HereBaseline::BuiltIn(crate::init_clt::RootPlan::NameRuleOnly),
            Plain::default(),
            HereOutput::Plain,
        )
        .err()
            == class(Unfinished::Other)
    );
    assert!(
        no_console
            .written(crate::root_steps::ADMITTED_NAME_RULE)
            .is_none()
    );

    // Apple's catalog out of reach waits in the same process until an explicit Stop.
    let unreached = mac();
    unreached
        .on("/usr/bin/xcode-select -p", none_selected())
        .on(catalog, Ran::exit(1, "catalog network unavailable"));
    unreached.on_sleep(|sys, _| {
        sys.stop_now();
    });
    assert!(plain(&unreached).err() == class(Unfinished::Stopped));

    // Another install holds the placeholder; the same authorized run waits until Stop.
    let busy = mac();
    busy.on("/usr/bin/xcode-select -p", none_selected())
        .running(&["/usr/sbin/softwareupdate"])
        .file(crate::items::developer::PLACEHOLDER, b"");
    busy.on_sleep(|sys, _| {
        sys.stop_now();
    });
    assert!(plain(&busy).err() == class(Unfinished::Stopped));
    assert_eq!(
        busy.written(crate::items::developer::PLACEHOLDER),
        Some(Vec::new()),
        "Stop preserves the other installer's marker"
    );

    // A baseline file the person's account wrote is never what the root half applies: one
    // that asks for nothing still installs the tools.
    let planted = mac();
    planted
        .file(baseline, b"schema = 1\n")
        .on("/usr/bin/xcode-select -p", none_selected())
        .on(catalog, Ran::exit(1, "catalog network unavailable"));
    planted.on_sleep(|sys, _| {
        sys.stop_now();
    });
    assert!(plain(&planted).err() == class(Unfinished::Stopped));

    // What the root half cannot even start is said as the other class, never in its words.
    let mut user = Fake::default();
    user.euid = 501;
    user.file(baseline, b"schema = 1\n");
    assert!(plain(&user).err() == class(Unfinished::Other));
    assert!(
        here_report(
            &user,
            Mode::Apply,
            HereBaseline::File(Path::new(baseline)),
            Plain::default(),
            HereOutput::Report
        )
        .err()
            == Some(ExitCode::from(2)),
        "a person's own run keeps its usage refusal"
    );
    let unread = Fake::root();
    assert!(plain(&unread).err() == class(Unfinished::Other));

    // The accepted root episode must retain the broker after publishing
    // its files. Completed tools cannot turn a transient bootstrap failure
    // into a successful root transaction with no helper to reach the LAN.
    let source = Path::new("/Applications/aterm.app");
    let image = aterm_uds::lan::install::test_image(0x0100_000c, &[b"this build"]);
    let broker_case = || {
        let mut sys = mac();
        with_tools(&sys);
        sys.running_app = Some(source.to_path_buf());
        sys.file("/Applications/aterm.app/Contents/MacOS/aterm", &image);
        sys.stat_of(
            source.to_str().unwrap(),
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        sys.app_check(|_, _| Ok(()));
        for (path, ino) in [
            (aterm_uds::lan::IMAGE_BUNDLE, 1),
            (aterm_uds::lan::PLIST_PATH, 2),
        ] {
            sys.file_id_of(
                Path::new(path),
                crate::port::FileId {
                    dev: 1,
                    ino,
                    born_ns: None,
                },
            );
        }
        sys
    };
    let broker = broker_case();
    const BROKER_PRINT: &str = "/bin/launchctl print system/systems.alab.aterm-lan";
    const BROKER_BOOTSTRAP: &str =
        "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist";
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    broker.simulate({
        let attempts = std::sync::Arc::clone(&attempts);
        move |_, command| match command.display().as_str() {
            BROKER_PRINT => Some(if attempts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                Ran::exit(113, "Could not find service")
            } else {
                Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n")
            }),
            BROKER_BOOTSTRAP => Some(
                if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Ran::exit(5, "transient bootstrap failure")
                } else {
                    Ran::ok("")
                },
            ),
            _ => None,
        }
    });
    broker
        .on_lan_broker(Err("the published broker is still starting".into()))
        .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
            protocol: aterm_uds::lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: Some("a direct path is not the installed helper".into()),
        }))
        .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
            protocol: aterm_uds::lan::PROTOCOL,
            build: "a valid newer source build".into(),
            direct: None,
        }))
        .on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Library/Developer/CommandLineTools\n"),
        )
        .on(
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            "/bin/launchctl kickstart system/systems.alab.aterm-lan",
            Ran::ok(""),
        );
    let ended = plain(&broker);
    let reported_success = matches!(ended, Ok(None));
    let published = crate::items::lan::reading(&broker).unwrap().unwrap();
    assert!(published.current());
    assert!(!published.owed());
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the same accepted root episode must retry bootstrap before reporting success={reported_success}; \
         files are current but the first bootstrap failed; journal={:?}",
        broker.written(Fake::JOURNAL)
    );
    assert!(reported_success);
    assert_eq!(
        broker
            .calls()
            .iter()
            .filter(|call| call.starts_with("clone "))
            .count(),
        1,
        "launch/readiness recovery keeps the already published image"
    );

    // Already healthy: neither image publication nor launchd mutation is owed.
    let healthy = broker_case();
    healthy
        .file(aterm_uds::lan::IMAGE_EXECUTABLE, &image)
        .file(
            aterm_uds::lan::PLIST_PATH,
            aterm_uds::lan::install::plist_text(source).as_bytes(),
        )
        .on(
            BROKER_PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
        )
        .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
            protocol: aterm_uds::lan::PROTOCOL,
            build: "a valid already-serving build".into(),
            direct: None,
        }));
    assert!(matches!(plain(&healthy), Ok(None)));
    assert!(
        !healthy.calls().iter().any(|call| call.starts_with("clone ")
            || call.starts_with("/bin/launchctl bootstrap")
            || call.starts_with("/bin/launchctl bootout")
            || call.starts_with("/bin/launchctl kickstart"))
    );

    for replaced in [false, true] {
        let interrupted = broker_case();
        interrupted
            .on(BROKER_PRINT, Ran::exit(113, "Could not find service"))
            .on(
                "/bin/launchctl enable system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on(
                BROKER_BOOTSTRAP,
                Ran::exit(5, "bootstrap still unavailable"),
            );
        interrupted.on_sleep(move |sys, _| {
            if replaced {
                sys.file(
                    aterm_uds::lan::IMAGE_EXECUTABLE,
                    &aterm_uds::lan::install::test_image(
                        0x0100_000c,
                        &[b"a newer root owner's image"],
                    ),
                );
            } else {
                sys.stop_now();
            }
        });
        assert!(
            plain(&interrupted).err()
                == class(if replaced {
                    Unfinished::Other
                } else {
                    Unfinished::Stopped
                })
        );
        assert_eq!(
            interrupted
                .calls()
                .iter()
                .filter(|call| *call == BROKER_BOOTSTRAP)
                .count(),
            1
        );
        assert_eq!(
            interrupted
                .calls()
                .iter()
                .filter(|call| call.starts_with("clone "))
                .count(),
            1
        );
    }

    // Another root owner can publish after our final ownership operation,
    // before any post-publication read. It must never become our receipt.
    let overwritten = broker_case();
    let other_image =
        aterm_uds::lan::install::test_image(0x0100_000c, &[b"another root owner's code"]);
    let other_plist = aterm_uds::lan::install::plist_text(Path::new("/Applications/other.app"));
    overwritten.on_chown({
        let (other_image, other_plist) = (other_image.clone(), other_plist.clone());
        move |sys, path| {
            if path == Path::new(aterm_uds::lan::PLIST_PATH) {
                sys.file(aterm_uds::lan::IMAGE_EXECUTABLE, &other_image)
                    .file(aterm_uds::lan::PLIST_PATH, other_plist.as_bytes());
            }
        }
    });
    overwritten
        .on(
            BROKER_PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
        )
        .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
            protocol: aterm_uds::lan::PROTOCOL,
            build: "another-build".into(),
            direct: None,
        }))
        .on(
            "/bin/launchctl bootout system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            BROKER_BOOTSTRAP,
            Ran::exit(5, "the other owner's job must not be restarted"),
        );
    overwritten.on_sleep(|sys, _| {
        sys.stop_now();
    });
    let ended = plain(&overwritten);
    assert!(
        !overwritten
            .calls()
            .iter()
            .any(|call| call.starts_with("/bin/launchctl bootout")
                || call.starts_with("/bin/launchctl bootstrap")),
        "another owner's publication is never adopted as our own: {:?}",
        overwritten.calls()
    );
    assert!(ended.err() == class(Unfinished::Other));
    assert_eq!(
        overwritten.written(aterm_uds::lan::IMAGE_EXECUTABLE),
        Some(other_image)
    );
    assert_eq!(
        overwritten.written(aterm_uds::lan::PLIST_PATH),
        Some(other_plist.into_bytes())
    );

    // Strict code refusal is terminal before either publication or launch.
    let refused = broker_case();
    refused.app_check(|_, _| {
        Err(aterm_keeper::identity::CheckError::Refused(
            "different designated identity".into(),
        ))
    });
    assert!(plain(&refused).err() == class(Unfinished::Other));
    assert!(refused.written(aterm_uds::lan::IMAGE_EXECUTABLE).is_none());
    assert!(
        !refused
            .calls()
            .iter()
            .any(|call| call.starts_with("exchange ")
                || call.starts_with("/bin/launchctl bootstrap"))
    );

    let serves = |sys: &Fake| {
        sys.on(BROKER_PRINT, Ran::exit(113, "Could not find service"))
            .on(BROKER_PRINT, Ran::exit(113, "Could not find service"))
            .on(
                BROKER_PRINT,
                Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
            )
            .on(
                "/bin/launchctl enable system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on(BROKER_BOOTSTRAP, Ran::ok(""))
            .on(
                "/bin/launchctl kickstart system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "a valid staged build".into(),
                direct: None,
            }));
    };
    let payload = "Contents/Resources/payload";
    for unanswered in [false, true] {
        let recovering = broker_case();
        recovering.file(source.join(payload).to_str().unwrap(), b"initial");
        let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        recovering.app_check({
            let checks = std::sync::Arc::clone(&checks);
            move |sys, candidate| {
                let first = checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                if unanswered && first {
                    return Err(aterm_keeper::identity::CheckError::Unanswered(
                        "signing service silent".into(),
                    ));
                }
                if !unanswered && sys.read_file(&candidate.join(payload)).unwrap() != b"repaired" {
                    return Err(aterm_keeper::identity::CheckError::Unchecked(
                        "resource check incomplete (-67054)".into(),
                    ));
                }
                Ok(())
            }
        });
        recovering.on_sleep(move |sys, _| {
            if !unanswered {
                sys.file(source.join(payload).to_str().unwrap(), b"repaired");
            }
        });
        serves(&recovering);
        assert!(matches!(plain(&recovering), Ok(None)));
        assert_eq!(
            recovering
                .read_file(&source.join(aterm_uds::lan::install::BUNDLE_EXECUTABLE))
                .unwrap(),
            image
        );
        assert_eq!(
            recovering.written(&format!("{}/{payload}", aterm_uds::lan::IMAGE_BUNDLE)),
            Some(if unanswered {
                b"initial".to_vec()
            } else {
                b"repaired".to_vec()
            })
        );
        assert_eq!(
            recovering
                .calls()
                .iter()
                .filter(|call| call.starts_with("clone "))
                .count(),
            if unanswered { 1 } else { 2 }
        );
        assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    let moved_source = broker_case();
    let successor = aterm_uds::lan::install::test_image(0x0100_000c, &[b"valid successor"]);
    let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    moved_source.app_check({
        let checks = std::sync::Arc::clone(&checks);
        move |_, _| {
            if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Err(aterm_keeper::identity::CheckError::Unanswered(
                    "service silent while source updates".into(),
                ))
            } else {
                Ok(())
            }
        }
    });
    moved_source.on_sleep({
        let successor = successor.clone();
        move |sys, _| {
            sys.file(
                source
                    .join(aterm_uds::lan::install::BUNDLE_EXECUTABLE)
                    .to_str()
                    .unwrap(),
                &successor,
            );
        }
    });
    serves(&moved_source);
    assert!(matches!(plain(&moved_source), Ok(None)));
    assert_eq!(
        moved_source.written(aterm_uds::lan::IMAGE_EXECUTABLE),
        Some(successor)
    );
    assert_eq!(
        moved_source
            .calls()
            .iter()
            .filter(|call| call.starts_with("clone "))
            .count(),
        2
    );
    assert_eq!(
        checks.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the obsolete first candidate is not checked again"
    );

    // An unreadable prior code directory is repairable; it need not be valid
    // code merely to bind its inode before replacing the old bundle.
    let malformed = broker_case();
    malformed.stat_of(
        aterm_uds::lan::IMAGE_BUNDLE,
        crate::port::Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    malformed.file(aterm_uds::lan::IMAGE_EXECUTABLE, b"malformed old image");
    serves(&malformed);
    assert!(matches!(plain(&malformed), Ok(None)));
    assert_eq!(
        malformed.written(aterm_uds::lan::IMAGE_EXECUTABLE),
        Some(image.clone())
    );

    // A foreign root publication during a slow code check must not be
    // overwritten by the now-obsolete unadmitted candidate.
    let overtaken = broker_case();
    let replacement = aterm_uds::lan::install::test_image(0x0100_000c, &[b"foreign root"]);
    overtaken.app_check({
        let replacement = replacement.clone();
        move |sys, _| {
            sys.stat_of(
                aterm_uds::lan::IMAGE_BUNDLE,
                crate::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o755,
                    dir: true,
                    symlink: false,
                    len: 0,
                },
            );
            sys.file(aterm_uds::lan::IMAGE_EXECUTABLE, &replacement)
                .file(aterm_uds::lan::PLIST_PATH, b"foreign job");
            sys.file_id_of(
                Path::new(aterm_uds::lan::IMAGE_BUNDLE),
                crate::port::FileId {
                    dev: 1,
                    ino: 99,
                    born_ns: None,
                },
            );
            sys.file_id_of(
                Path::new(aterm_uds::lan::PLIST_PATH),
                crate::port::FileId {
                    dev: 1,
                    ino: 100,
                    born_ns: None,
                },
            );
            Ok(())
        }
    });
    assert!(plain(&overtaken).err() == class(Unfinished::Other));
    assert_eq!(
        overtaken.written(aterm_uds::lan::IMAGE_EXECUTABLE),
        Some(replacement)
    );
    assert_eq!(
        overtaken.written(aterm_uds::lan::PLIST_PATH),
        Some(b"foreign job".to_vec())
    );
    assert!(
        !overtaken
            .calls()
            .iter()
            .any(|call| call.starts_with("exchange ")
                || call.starts_with("/bin/launchctl bootstrap"))
    );

    let stopped_check = broker_case();
    stopped_check.app_check(|sys, _| {
        sys.stop_now();
        Ok(())
    });
    assert!(plain(&stopped_check).err() == class(Unfinished::Stopped));
    assert!(
        stopped_check
            .written(aterm_uds::lan::IMAGE_EXECUTABLE)
            .is_none()
    );
}

/// `--name` gives the Mac its swarm label as Computer Name, Local hostname and host name
/// under the one password the tools take, journaled; a name macOS refuses leaves the exit
/// saying the tools alone. NEGATIVE CONTROLS: with no name the root half runs no `scutil
/// --set`, and with the tools out of reach the exit is still theirs.
#[test]
fn here_apply_plain_names_the_mac_and_exits_on_the_tools_alone() {
    use crate::init_clt::Unfinished;
    use aterm_types::machine_name::{GeneratedNames, Number};
    let target = GeneratedNames::number_only(Number::parse("m42").unwrap());
    let console = crate::port::Stat {
        uid: 0,
        gid: 0,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: 0,
    };
    let tools = || {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/mdutil -s /System/Volumes/Data",
            Ran::ok("Indexing disabled."),
        );
        sys.stat_of("/dev/console", console);
        for executable in [
            crate::items::developer::CLT_GIT,
            crate::items::developer::CLT_CLANG,
        ] {
            sys.stat_of(
                executable,
                crate::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o755,
                    dir: false,
                    symlink: false,
                    len: 1,
                },
            );
        }
        sys.on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Library/Developer/CommandLineTools\n"),
        );
        sys
    };
    let sets = |sys: &Fake| -> Vec<String> {
        sys.calls()
            .into_iter()
            .filter(|call| call.contains("scutil --set"))
            .collect()
    };
    let plain = |sys: &Fake, name| {
        let given = Plain {
            name,
            leave_developer_mode: false,
        };
        here_report(
            sys,
            Mode::Apply,
            HereBaseline::BuiltIn(crate::init_clt::RootPlan::WithTools),
            given,
            HereOutput::Plain,
        )
    };
    let journal =
        |sys: &Fake| String::from_utf8(sys.written(Fake::JOURNAL).unwrap_or_default()).unwrap();

    // Read three times each: the Mac's facts, the probe, and the check after the set.
    let named = tools();
    named
        .on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("Andrew\u{2019}s MacBook Pro\n"),
        )
        .on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("Andrew\u{2019}s MacBook Pro\n"),
        )
        .on("/usr/sbin/scutil --get ComputerName", Ran::ok("m42\n"))
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("Andrews-MacBook-Pro\n"),
        )
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("Andrews-MacBook-Pro\n"),
        )
        .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"))
        .on(
            "/usr/sbin/scutil --get HostName",
            Ran::exit(1, "HostName: not set\n"),
        )
        .on("/usr/sbin/scutil --get HostName", Ran::ok("m42\n"))
        .on("/usr/sbin/scutil --set ComputerName m42", Ran::ok(""))
        .on("/usr/sbin/scutil --set LocalHostName m42", Ran::ok(""))
        .on("/usr/sbin/scutil --set HostName m42", Ran::ok(""));
    assert!(matches!(plain(&named, Some(&target)), Ok(None)));
    assert_eq!(
        sets(&named),
        [
            "/usr/sbin/scutil --set ComputerName m42",
            "/usr/sbin/scutil --set LocalHostName m42",
            "/usr/sbin/scutil --set HostName m42"
        ]
    );
    let said = journal(&named);
    assert!(said.contains("names.computer-name"), "{said}");
    assert!(said.contains("names.local-host-name"), "{said}");
    assert!(said.contains("names.host-name"), "{said}");

    let generated = aterm_types::machine_name::GeneratedNames::new(
        Number::parse("m85").unwrap(),
        &aterm_types::machine_name::Nickname::parse("Dog").unwrap(),
    );
    let dog = tools();
    let mut expected = Vec::new();
    for pref in crate::root_steps::NAME_PREFS {
        let slot = aterm_types::machine_name::OsSlot::from_macos_preference(pref).unwrap();
        let value = generated.for_slot(slot).unwrap();
        let get = format!("/usr/sbin/scutil --get {pref}");
        for _ in 0..if pref == "HostName" { 1 } else { 2 } {
            dog.on(&get, Ran::ok("old"));
        }
        dog.on(&get, Ran::ok(value));
        let set = crate::port::Cmd::new(crate::items::names::SCUTIL)
            .args(["--set", pref, value])
            .display();
        dog.on(&set, Ran::ok(""));
        expected.push(set);
    }
    assert!(matches!(plain(&dog, Some(&generated)), Ok(None)));
    assert_eq!(sets(&dog), expected);
    assert!(
        !dog.calls()
            .iter()
            .any(|call| call.contains("com.apple.smb.server") || call.contains("scutil --prefs")),
        "generated naming must not touch SMB preferences: {:?}",
        dog.calls()
    );

    // Already named: nothing is set again.
    let already = tools();
    already
        .on("/usr/sbin/scutil --get ComputerName", Ran::ok("m42\n"))
        .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"));
    assert!(matches!(plain(&already, Some(&target)), Ok(None)));
    assert_eq!(sets(&already), Vec::<String>::new());

    // macOS refuses the name: the tools' exit stands.
    let refused = tools();
    refused
        .on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("Andrew\u{2019}s MacBook Pro\n"),
        )
        .on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("Andrews-MacBook-Pro\n"),
        )
        .on(
            "/usr/sbin/scutil --set ComputerName m42",
            Ran::exit(1, "SCPreferencesCommitChanges() failed"),
        )
        .on(
            "/usr/sbin/scutil --set LocalHostName m42",
            Ran::exit(1, "SCPreferencesCommitChanges() failed"),
        );
    assert!(matches!(plain(&refused, Some(&target)), Ok(None)));
    assert_eq!(sets(&refused).len(), 2, "{:?}", refused.calls());

    // NEGATIVE CONTROL: no name, no rename.
    let unnamed = tools();
    assert!(matches!(plain(&unnamed, None), Ok(None)));
    assert_eq!(sets(&unnamed), Vec::<String>::new());

    // Developer mode reading off is turned on, unless the run is told to leave it (a person
    // turned it off after setup turned it on): then nothing touches it.
    let enables = |leave_developer_mode: bool| {
        let sys = tools();
        sys.on(
            "/usr/sbin/DevToolsSecurity -status",
            Ran::ok("Developer mode is currently disabled.\n"),
        )
        .on("/usr/sbin/DevToolsSecurity -enable", Ran::ok(""));
        let given = Plain {
            name: None,
            leave_developer_mode,
        };
        assert!(matches!(
            here_report(
                &sys,
                Mode::Apply,
                HereBaseline::BuiltIn(crate::init_clt::RootPlan::WithTools),
                given,
                HereOutput::Plain
            ),
            Ok(None) | Err(_)
        ));
        sys.calls()
            .iter()
            .any(|call| call == "/usr/sbin/DevToolsSecurity -enable")
    };
    assert!(enables(false), "CONTROL: setup's own run turns it on");
    assert!(!enables(true), "a run told to leave it never turns it on");

    // An unavailable catalog retains custody beside the completed naming steps.
    let unreached = Fake::root();
    unreached.born(std::process::id(), 123).running(&[]);
    unreached
        .stat_of("/dev/console", console)
        .on(
            "/usr/bin/xcode-select -p",
            Ran::exit(
                2,
                "xcode-select: error: Unable to get active developer directory.",
            ),
        )
        .on(
            "/usr/sbin/softwareupdate -l",
            Ran::exit(1, "catalog network unavailable"),
        )
        .on("/usr/sbin/scutil --get ComputerName", Ran::ok("m42\n"))
        .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"));
    unreached.on_sleep(|sys, _| {
        sys.stop_now();
    });
    assert!(
        plain(&unreached, Some(&target)).err()
            == Some(ExitCode::from(Unfinished::Stopped.code().unwrap()))
    );
}

#[test]
fn here_journals_each_change_in_the_journal_of_the_system_it_runs_on() {
    let journaled = |sys: &Fake| -> Vec<aterm_json::Value> {
        sys.written(Fake::JOURNAL)
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .unwrap_or_default()
            .lines()
            .map(|l| aterm_json::from_str(l).unwrap())
            .collect()
    };
    let baseline = "/b/baseline.toml";
    let text: &[u8] = b"schema = 1\n[names]\nhost_name = \"lab-07.lab.example\"\n";
    let mac = || {
        let sys = Fake::root();
        sys.file(baseline, text).stat_of(
            "/dev/console",
            crate::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys
    };
    let sys = mac();
    sys.on(
        "/usr/sbin/scutil --get HostName",
        Ran::ok("lab-07.lab.example\n"),
    );
    assert_eq!(
        here(
            &sys,
            Mode::Apply,
            HereBaseline::File(Path::new(baseline)),
            Plain::default(),
            HereOutput::Json
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(
        journaled(&sys),
        Vec::<aterm_json::Value>::new(),
        "a line for a run that changed nothing"
    );
    assert!(
        !sys.calls().iter().any(|call| call.contains("hidutil")),
        "a baseline with no keyboard table must not search live devices: {:?}",
        sys.calls()
    );
    let sys = mac();
    sys.on(
        "/usr/sbin/scutil --get HostName",
        Ran::exit(1, "HostName: not set"),
    )
    .on(
        "/usr/sbin/scutil --get HostName",
        Ran::ok("lab-07.lab.example\n"),
    )
    .on(
        "/usr/sbin/scutil --set HostName lab-07.lab.example",
        Ran::ok(""),
    );
    assert_eq!(
        here(
            &sys,
            Mode::Apply,
            HereBaseline::File(Path::new(baseline)),
            Plain::default(),
            HereOutput::Json
        ),
        ExitCode::SUCCESS
    );
    let lines = journaled(&sys);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["id"].as_str(), Some("names.host-name"));
    assert_eq!(line["from"].as_str(), Some("not set"));
    assert_eq!(line["to"].as_str(), Some("\"lab-07.lab.example\""));
    assert_eq!(
        line["revert"].as_str(),
        Some("/usr/sbin/scutil --set HostName ''")
    );
}

#[test]
fn monitor_tab_opener_speaks_silent_auth_and_explicit_socket_tokens() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let dir = aterm_tempfile::Builder::new()
        .prefix("at-mon-open-")
        .tempdir_in("/tmp")
        .unwrap();
    let sym_sock = dir.path().join("aterm-1111.sock");
    let real_tok = dir.path().join("real.token");
    std::fs::write(&real_tok, "secret1111\n").unwrap();
    std::os::unix::fs::symlink(&real_tok, dir.path().join("aterm-1111.token")).unwrap();
    let _unused = UnixListener::bind(&sym_sock).unwrap();

    let explicit_sock = dir.path().join("custom.sock");
    std::fs::write(dir.path().join("custom.sock.token"), "cafef00d\n").unwrap();
    let listener = UnixListener::bind(&explicit_sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut conn, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut auth = String::new();
        reader.read_line(&mut auth).unwrap();
        assert_eq!(auth, "AUTH cafef00d\n");
        let mut verb = String::new();
        reader.read_line(&mut verb).unwrap();
        assert_eq!(verb, "open app settings /monitor\n");
        conn.write_all(b"OK\n").unwrap();
    });

    // The server above always answers: a hang detector, never the opener's 250 ms.
    let opened = open_native_monitor_tab_in_candidates(
        &[sym_sock, explicit_sock.clone()],
        Duration::from_secs(60),
    );
    server.join().unwrap();
    assert_eq!(opened.as_deref(), explicit_sock.to_str());
    assert_eq!(
        device_info(&[
            b"model=Mac16,1".to_vec(),
            b"osxvers=26".to_vec(),
            b"x".to_vec()
        ]),
        (Some("Mac16,1".to_owned()), Some(26))
    );
    assert_eq!(device_info(&[b"model=".to_vec()]), (None, None));
}

/// A window name a host reports reaches the owner's terminal escaped, as the supervisor
/// beside it does. NEGATIVE CONTROL: a plain window name prints as it is.
#[test]
fn a_host_s_window_name_cannot_write_to_the_owner_s_terminal() {
    let tab = |window: &str| crate::protocol::HostedSession {
        instance_pid: 99,
        window: window.into(),
        focused: true,
        ..crate::protocol::HostedSession::default()
    };
    let hostile = tab("0\x1b[2J\x1b]52;c;cHdu\x07");
    let line = instance_header(99, &[&hostile], "80\u{d7}24");
    assert!(!line.contains('\x1b') && !line.contains('\x07'), "{line:?}");
    assert!(line.contains("window 0\\u{1b}[2J"), "{line:?}");
    let plain = tab("3");
    let line = instance_header(99, &[&plain], "80\u{d7}24");
    assert!(
        line.contains("aterm-99 \u{b7} window 3 (focused) \u{b7} grid 80\u{d7}24"),
        "{line:?}"
    );
}

/// A host that answers with its UUID in another case is the host its inventory names (the
/// UUID is hex). NEGATIVE CONTROL: a host that answers with another UUID is not.
#[test]
fn hello_matches_the_host_s_uuid_in_either_case() {
    let host = lab(1, &UUID_1.to_ascii_lowercase());
    let sys = controller(&[&host], false);
    let mut hello = hello_of(&host);
    hello.uuid = UUID_1.to_owned();
    sys.on(
        &request(&host, "hello"),
        Ran::ok(&aterm_json::to_string(&hello).unwrap()),
    );
    assert_eq!(ask_hello(&env(&sys), &host).unwrap().uuid, UUID_1);

    let sys = controller(&[&host], false);
    hello.uuid = UUID_2.to_owned();
    sys.on(
        &request(&host, "hello"),
        Ran::ok(&aterm_json::to_string(&hello).unwrap()),
    );
    let refused = ask_hello(&env(&sys), &host).unwrap_err();
    assert!(
        matches!(&refused, HelloError::Answer(why) if why.contains(&format!("it answers as {UUID_2}"))),
        "{refused:?}"
    );
}

/// The agent socket the walk loads the key into, and every envelope it seals, is
/// [`Env::agent_sock`], never this process's own `SSH_AUTH_SOCK`: each run is handed exactly
/// the injected socket. NEGATIVE CONTROL: with no socket a walk's key with a passphrase is
/// refused before `ssh-add` runs.
#[test]
fn a_walk_loads_and_seals_through_the_env_s_agent_socket() {
    let sys = Fake::root();
    let key = format!("{ROOT}/keys/signing");
    sys.on(
        &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
        Ran::exit(
            255,
            "Load key: incorrect passphrase supplied to decrypt private key\n",
        ),
    );
    let held = format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -T {key}.pub");
    sys.on(&held, Ran::exit(1, ""));
    sys.on(
        &format!("tty SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -t 3600 {key}"),
        Ran::ok(""),
    );
    sys.on(
        &format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -d {key}"),
        Ran::ok(""),
    );
    let handed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&handed);
    sys.simulate(move |_, cmd| {
        seen.lock().unwrap().push((cmd.display(), cmd.env.clone()));
        None
    });
    let seen = std::sync::Arc::clone(&handed);
    sys.tty_model(move |_, cmd| {
        seen.lock()
            .unwrap()
            .push((format!("tty {}", cmd.display()), cmd.env.clone()));
        None
    });
    let mut e = env(&sys);
    e.agent_sock = Some("/private/walk-agent.sock".into());
    let (how, loaded) = person_signing(&e).unwrap();
    assert_eq!(how, Signing::Agent);
    assert!(loaded.is_some());
    let sign = format!(
        "SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-keygen -Y sign -U -f {key}.pub -n aterm-machines"
    );
    let body = Envelope {
        schema: crate::SCHEMA,
        host: UUID_1.into(),
        host_name: "lab-01".into(),
        serial: 1,
        issued: 10,
        expires: 10 + ENVELOPE_LIFETIME,
        action: Action::Plan,
        disrupt: Disrupt::WhenIdle,
        idle_secs: 900,
        baseline: None,
        artifacts: Vec::new(),
        exec: None,
        upgrade: None,
        rekey: None,
    };
    let unsigned = seal_with(&e, &[body], Path::new(&key), how).unwrap_err();
    assert!(unsigned.contains("did not sign"), "{unsigned}");
    drop(loaded);
    let handed = handed.lock().unwrap();
    let sock = vec![(
        "SSH_AUTH_SOCK".to_owned(),
        "/private/walk-agent.sock".to_owned(),
    )];
    for want in [
        held.clone(),
        format!("tty SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -t 3600 {key}"),
        sign,
        format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -d {key}"),
    ] {
        let ran: Vec<_> = handed
            .iter()
            .filter(|(line, _)| line.starts_with(&want))
            .collect();
        assert!(!ran.is_empty(), "{want}: {handed:#?}");
        assert!(ran.iter().all(|(_, env)| *env == sock), "{want}: {ran:#?}");
    }

    let sys = Fake::root();
    sys.on(
        &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
        Ran::exit(
            255,
            "Load key: incorrect passphrase supplied to decrypt private key\n",
        ),
    );
    let refused = person_signing(&env(&sys)).map(|(how, _)| how).unwrap_err();
    assert!(refused.contains("no ssh-agent"), "{refused}");
    assert!(
        !sys.calls().iter().any(|c| c.contains("ssh-add")),
        "{:?}",
        sys.calls()
    );
}
