// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::Fake;

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

fn table(computer: Option<&str>, local: Option<&str>, host: Option<&str>) -> Table {
    Table {
        computer_name: computer.map(Into::into),
        local_host_name: local.map(Into::into),
        host_name: host.map(Into::into),
        admitted_name_rule: None,
    }
}

/// `plutil -extract NetworkServices xml1 -o - …/preferences.plist` in the VM (27.0): three
/// services, none keeping a NetBIOS name; and the same once its Ethernet service kept
/// `LAB-03-SVCPIN` (set with `scutil --prefs`, the name then held through a rename and a
/// restart).
const SERVICES: &str = include_str!("../../fixtures/sc/vm-27.0-network-services.plist");
const SERVICES_PINNED: &str =
    include_str!("../../fixtures/sc/vm-27.0-network-services-pinned.plist");
const ETHERNET: &str = "364A8646-D38B-4922-B8A2-DD9542E83781";
const SERVICE_IDS: [&str; 3] = [
    ETHERNET,
    "4B8335D7-375E-423C-823B-D06921276474",
    "6D5E0C47-9620-4E2D-B844-6B696CF4ED58",
];

/// The VM's services, each keeping `name`.
fn services_keeping(name: &str) -> String {
    SERVICES.replace("<key>SMB</key>\n\t\t<dict/>", &format!(
        "<key>SMB</key>\n\t\t<dict>\n\t\t\t<key>NetBIOSName</key>\n\t\t\t<string>{name}</string>\n\t\t</dict>"
    ))
}

fn services_read() -> String {
    services_cmd().display()
}

/// `scutil --get HostName` in the VM when it is unset: stderr, exit 1.
fn not_set(key: &str) -> Ran {
    Ran::exit(1, &format!("{key}: not set"))
}

#[test]
fn a_computer_name_brings_its_netbios_name_and_each_name_is_one_item() {
    let items = expand(
        &table(Some("lab-07"), Some("lab-07"), Some("lab-07.lab.example")),
        &ctx(),
    )
    .unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "names.computer-name",
            "names.netbios-name",
            "names.local-host-name",
            "names.host-name"
        ]
    );
}

#[test]
fn a_placeholder_the_controller_did_not_fill_is_refused() {
    let err = expand(&table(Some("{name}"), None, None), &ctx())
        .err()
        .unwrap();
    assert!(err.contains("placeholder"), "{err}");
    let err = expand(&table(None, Some("lab-{user}"), None), &ctx())
        .err()
        .unwrap();
    assert!(err.contains("local_host_name"), "{err}");
}

/// The Local hostname `local` asks for, as its item would set it.
fn local_of(local: &str, facts: HostFacts) -> Result<String, String> {
    let items = expand(&table(None, Some(local), None), &on(facts.clone()))?;
    let sys = Fake::root();
    sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"));
    match items[0].probe(&sys, &on(facts)) {
        Probe::Change { to, .. } => Ok(to.trim_matches('"').to_owned()),
        other => Err(format!("{other:?}")),
    }
}

#[test]
fn a_local_hostname_is_the_label_macos_would_make_of_it_and_never_one_scutil_refuses() {
    for (asked, set) in [
        ("bad name.x", "bad-name-x"),
        ("lab_03", "lab-03"),
        ("lab.03", "lab-03"),
        ("lab_03.b-{hardware}", "lab-03-b-MacBook-Air-M5-32GB-4TB"),
        ("-lab", "lab"),
        ("lab-", "lab"),
        ("Lab-07", "Lab-07"),
    ] {
        assert_eq!(local_of(asked, air()).as_deref(), Ok(set), "{asked:?}");
    }
    let long = "a".repeat(64);
    for bad in ["", "._.", long.as_str()] {
        assert!(
            expand(&table(None, Some(bad), None), &ctx()).is_err(),
            "{bad:?}"
        );
    }
    let items = expand(&described("lab_03.b"), &on(air())).unwrap();
    let sys = Fake::root();
    sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok("x\n"))
        .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"))
        .on(NETBIOS_READ, Ran::ok("X\n"))
        .on(&services_read(), Ran::ok(SERVICES));
    let to: Vec<String> = items
        .iter()
        .map(|i| match i.probe(&sys, &on(air())) {
            Probe::Change { to, .. } => to,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        to,
        [
            "\"lab_03.b MacBook Air M5 32GB 4TB\"",
            "\"LAB-03-B-MACBOO\"",
            "\"lab-03-b-MacBook-Air-M5-32GB-4TB\""
        ]
    );
    let air = crate::hardware::describe(&air()).unwrap();
    assert_eq!(
        planned_local_host_name("lab_03.b-{hardware}", Some(&air)).as_deref(),
        Some("lab-03-b-MacBook-Air-M5-32GB-4TB")
    );
    assert_eq!(planned_local_host_name("lab_03.b-{hardware}", None), None);
    assert_eq!(
        planned_local_host_name("lab_03", None).as_deref(),
        Some("lab-03")
    );
    assert_eq!(
        planned_local_host_name("{tga}-{hardware}", Some(&air)),
        None
    );
}

#[test]
fn a_host_name_or_computer_name_scutil_would_misread_is_refused_when_the_baseline_expands() {
    assert!(expand(&table(None, None, Some("lab_07.example")), &ctx()).is_err());
    assert!(expand(&table(Some(" "), None, None), &ctx()).is_err());
    for bad in ["-f/etc/sudoers", "-d", "-"] {
        assert!(
            expand(&table(Some(bad), None, None), &ctx()).is_err(),
            "{bad:?}"
        );
    }
    assert!(expand(&table(Some("Lab 7 - Room B"), None, None), &ctx()).is_ok());
}

#[test]
fn netbios_names_follow_the_local_hostname_rule_uppercased_to_fifteen_characters() {
    assert_eq!(netbios_name("lab-07").as_deref(), Some("LAB-07"));
    assert_eq!(
        netbios_name("Managed\u{2019}s Virtual Machine").as_deref(),
        Some("MANAGEDS-VIRTUA")
    );
    assert_eq!(
        netbios_name("Lab 7 \u{2014} Room B").as_deref(),
        Some("LAB-7-ROOM-B")
    );
    assert_eq!(
        netbios_name("abcdefghijklmn-pq").as_deref(),
        Some("ABCDEFGHIJKLMN")
    );
    assert_eq!(netbios_name("\u{2014}"), None);
}

#[test]
fn an_unset_hostname_is_absent_and_an_unanswered_get_is_unusable() {
    assert_eq!(read_scutil(&not_set("HostName")), Read::Absent);
    assert_eq!(
        read_scutil(&Ran::ok("Managed\u{2019}s Virtual Machine\n")),
        Read::Value("Managed\u{2019}s Virtual Machine".into())
    );
    assert!(matches!(
        read_scutil(&Ran::exit(1, "Permission denied.")),
        Read::Unusable(_)
    ));
}

#[test]
fn renaming_the_vm_sets_each_name_as_root_and_reverts_to_what_it_was() {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get ComputerName",
        Ran::ok("Managed\u{2019}s Virtual Machine\n"),
    )
    .on("/usr/sbin/scutil --get ComputerName", Ran::ok("lab-07\n"))
    .on("/usr/sbin/scutil --set ComputerName lab-07", Ran::ok(""))
    .on(
        "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
        Ran::ok("VIRTUALMAC-904D\n"),
    )
    .on(
        "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
        Ran::ok("LAB-07\n"),
    )
    .on(
        "/usr/bin/defaults write /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName -string LAB-07",
        Ran::ok(""),
    )
    .on(&services_read(), Ran::ok(SERVICES))
    .on(&services_read(), Ran::ok(SERVICES))
    .on(&services_read(), Ran::ok(&services_keeping("LAB-07")))
    .on("/usr/sbin/scutil --prefs", Ran::ok(""))
    .on("/usr/sbin/scutil --get HostName", not_set("HostName"))
    .on("/usr/sbin/scutil --get HostName", Ran::ok("lab-07.lab.example\n"))
    .on("/usr/sbin/scutil --set HostName lab-07.lab.example", Ran::ok(""));
    let items = expand(
        &table(Some("lab-07"), None, Some("lab-07.lab.example")),
        &ctx(),
    )
    .unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::Changed),
        "{rows:#?}"
    );
    assert_eq!(
        rows[0].revert.as_deref(),
        Some("/usr/sbin/scutil --set ComputerName 'Managed\u{2019}s Virtual Machine'")
    );
    assert!(sys.calls().contains(&"/usr/sbin/scutil --prefs".to_owned()));
    let netbios = rows[1].revert.as_deref().unwrap();
    assert!(
        netbios.starts_with(
            "/usr/bin/defaults write /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName -string VIRTUALMAC-904D && printf 'open\\nlock\\n"
        ) && netbios.ends_with("' | /usr/sbin/scutil --prefs"),
        "{netbios}"
    );
    for id in SERVICE_IDS {
        assert!(
            netbios.contains(&format!(
                "get /NetworkServices/{id}/SMB\\nd.remove NetBIOSName\\n"
            )),
            "{netbios}"
        );
    }
    assert_eq!(
        rows[2].revert.as_deref(),
        Some("/usr/sbin/scutil --set HostName ''")
    );
    assert_eq!(rows[2].from.as_deref(), Some("not set"));
}

#[test]
fn the_starter_baseline_takes_a_hand_set_host_name_off_with_the_rename() {
    let text = crate::baseline::fill(
        crate::controller::templates::DEFAULT_BASELINE,
        &[("name", "m201"), ("tag", "m201")],
    )
    .unwrap();
    let names = crate::baseline::parse(&text).unwrap().names.unwrap();
    let host: Vec<Box<dyn Item>> = expand(&names, &ctx())
        .unwrap()
        .into_iter()
        .filter(|i| i.id() == "names.host-name")
        .collect();
    assert_eq!(host.len(), 1, "the starter [names] leaves HostName alone");
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get HostName",
        Ran::ok("m201-Air-M2-24GB-1TB-2\n"),
    )
    .on("/usr/sbin/scutil --get HostName", not_set("HostName"))
    .on("/usr/sbin/scutil --set HostName ''", Ran::ok(""));
    let rows = engine::run(&host, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
    assert!(
        sys.calls()
            .contains(&"/usr/sbin/scutil --set HostName ''".to_owned())
    );
}

#[test]
fn an_empty_host_name_unsets_it_and_an_unset_one_is_already_as_wanted() {
    let sys = Fake::root();
    sys.on("/usr/sbin/scutil --get HostName", not_set("HostName"));
    let items = expand(&table(None, None, Some("")), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(rows[0].to.as_deref(), Some("not set"));
}

#[test]
fn a_name_that_did_not_take_is_a_failure_and_scutils_refusal_is_its_reason() {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("Manageds-Virtual-Machine\n"),
    )
    .on(
        "/usr/sbin/scutil --set LocalHostName lab-07",
        Ran::exit(1, "Permission denied."),
    );
    let items = expand(&table(None, Some("lab-07"), None), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .contains("Permission denied.")
    );
}

#[test]
fn a_netbios_name_macos_rewrote_in_lower_case_is_the_name_wanted() {
    let sys = Fake::root();
    sys.on(
        "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
        Ran::ok("lab-02\n"),
    )
    .on(&services_read(), Ran::ok(&services_keeping("lab-02")));
    let item = NetBios::new(Ok("LAB-02".into()));
    assert!(matches!(item.probe(&sys, &ctx()), Probe::Ok { .. }));
}

#[test]
fn a_missing_netbios_name_is_absent_and_reverts_by_deleting_it() {
    let missing = Ran::exit(
        1,
        "Error: Could not find key 'NetBIOSName' in domain 'SystemConfiguration/com.apple.smb.server'.",
    );
    assert_eq!(read_defaults_string(&missing), Read::Absent);
    let sys = Fake::root();
    sys.on(
        "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName",
        missing,
    )
    .on(&services_read(), Ran::ok(&services_keeping("LAB-07")));
    let item = NetBios::new(Ok("LAB-07".into()));
    assert_eq!(
        item.probe(&sys, &ctx()),
        Probe::Change {
            from: "not set".into(),
            to: "\"LAB-07\"".into()
        }
    );
    assert_eq!(
        item.revert().as_deref(),
        Some(
            "/usr/bin/defaults delete /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName"
        )
    );
}

#[test]
fn a_netbios_name_no_network_service_keeps_is_a_change_macos_would_undo_at_restart() {
    let sys = Fake::root();
    sys.on(NETBIOS_READ, Ran::ok("LAB-03\n"))
        .on(&services_read(), Ran::ok(SERVICES));
    let item = NetBios::new(Ok("LAB-03".into()));
    assert_eq!(
        item.probe(&sys, &ctx()),
        Probe::Change {
            from: "\"LAB-03\" until a restart (no network service keeps it)".into(),
            to: "\"LAB-03\"".into()
        }
    );
    let sys = Fake::root();
    sys.on(NETBIOS_READ, Ran::ok("LAB-03-SVCPIN\n"))
        .on(&services_read(), Ran::ok(SERVICES_PINNED));
    assert_eq!(
        parse_services(SERVICES_PINNED.as_bytes()).unwrap()[0],
        (ETHERNET.to_owned(), Some("LAB-03-SVCPIN".to_owned()))
    );
    let item = NetBios::new(Ok("LAB-03-SVCPIN".into()));
    assert!(matches!(item.probe(&sys, &ctx()), Probe::Change { .. }));
    let sys = Fake::root();
    sys.on(NETBIOS_READ, Ran::ok("LAB-03\n"));
    assert!(matches!(
        NetBios::new(Ok("LAB-03".into())).probe(&sys, &ctx()),
        Probe::Unusable { .. }
    ));
    let sys = Fake::root();
    sys.on(NETBIOS_READ, Ran::exit(2, "defaults: Permission denied"))
        .on(&services_read(), Ran::ok(SERVICES));
    assert!(matches!(
        NetBios::new(Ok("LAB-03".into())).probe(&sys, &ctx()),
        Probe::Unusable { .. }
    ));
}

#[test]
fn the_netbios_name_is_kept_in_each_services_smb_settings_with_the_rest_of_them() {
    let ids: Vec<(String, Option<String>)> = parse_services(SERVICES.as_bytes()).unwrap();
    assert_eq!(
        ids.iter()
            .map(|(id, p)| (id.as_str(), p.clone()))
            .collect::<Vec<_>>(),
        SERVICE_IDS.map(|id| (id, None))
    );
    let script = pin_script(SERVICE_IDS.map(|id| (id, Some("M14-MACBOOK-AIR")))).unwrap();
    let mut want = String::from("open\nlock\n");
    for id in SERVICE_IDS {
        want.push_str(&format!(
            "d.init\nget /NetworkServices/{id}/SMB\nd.add NetBIOSName M14-MACBOOK-AIR\nset /NetworkServices/{id}/SMB\n"
        ));
    }
    want.push_str("commit\napply\nunlock\nclose\nquit\n");
    assert_eq!(script, want);
    assert_eq!(pin_script([(ETHERNET, Some("A B"))]), None);
    assert_eq!(pin_script([("x\nquit", Some("A"))]), None);
    assert!(
        parse_services(b"<dict><key>x\nset /</key><dict/></dict>")
            .unwrap_err()
            .contains("not a UUID")
    );
}

#[test]
fn a_name_that_could_not_be_read_is_unknown_never_the_default() {
    let items = expand(&table(Some("lab-07"), None, None), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut NoJournal);
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::Unknown),
        "{rows:#?}"
    );
    assert!(rows.iter().all(|r| r.revert.is_none()));
}

const NETBIOS_READ: &str = "/usr/bin/defaults read /Library/Preferences/SystemConfiguration/com.apple.smb.server NetBIOSName";

/// The MacBook Air's facts as `facts::gather` reads them (Mac17,4, macOS 27.0.1:
/// product-name "MacBook Air (15-inch, M5)", brand "Apple M5", hw.memsize 34359738368,
/// its SSD 4002222325760 bytes in ioreg).
fn air() -> HostFacts {
    HostFacts {
        model_name: "MacBook Air".into(),
        chip: "Apple M5".into(),
        memory_bytes: 34_359_738_368,
        disk_bytes: 4_002_222_325_760,
        ..HostFacts::default()
    }
}

fn on(facts: HostFacts) -> Ctx {
    Ctx { facts, ..ctx() }
}

/// `[names]` as the default baseline has it, `{tag}` filled by the controller.
fn described(tag: &str) -> Table {
    table(
        Some(&format!("{tag} {{hardware}}")),
        Some(&format!("{tag}-{{hardware}}")),
        None,
    )
}

#[test]
fn the_air_is_named_for_what_it_is_and_the_plan_shows_each_rename() {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get ComputerName",
        Ran::ok("m14 Air M5 32GB 4TB prime\n"),
    )
    .on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("m14-Air-M5-32GB-4TB-prime\n"),
    )
    .on(NETBIOS_READ, Ran::ok("m14AirM4TBprime\n"))
    .on(&services_read(), Ran::ok(SERVICES));
    let items = expand(&described("m14"), &on(air())).unwrap();
    let rows = engine::run(&items, Mode::Plan, &sys, &on(air()), &mut NoJournal);
    let changes: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            assert_eq!(r.outcome, Outcome::Planned, "{r:#?}");
            (
                r.id.as_str(),
                r.from.as_deref().unwrap(),
                r.to.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            (
                "names.computer-name",
                "\"m14 Air M5 32GB 4TB prime\"",
                "\"m14 MacBook Air M5 32GB 4TB\""
            ),
            (
                "names.netbios-name",
                "\"m14AirM4TBprime\"",
                "\"M14-MACBOOK-AIR\""
            ),
            (
                "names.local-host-name",
                "\"m14-Air-M5-32GB-4TB-prime\"",
                "\"m14-MacBook-Air-M5-32GB-4TB\""
            ),
        ]
    );
    assert!(!sys.calls().iter().any(|c| c.contains("--set")));
}

#[test]
fn bonjours_conflict_suffix_and_stale_specs_are_renamed_to_what_the_mac_reads() {
    let m1 = HostFacts {
        model_name: "MacBook Air".into(),
        chip: "Apple M2".into(),
        memory_bytes: 25_769_803_776,
        disk_bytes: 1_000_555_581_440,
        ..HostFacts::default()
    };
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("m1-Air-M2-24GB-1TB-2\n"),
    )
    .on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("m1-MacBook-Air-M2-24GB-1TB\n"),
    )
    .on(
        "/usr/sbin/scutil --set LocalHostName m1-MacBook-Air-M2-24GB-1TB",
        Ran::ok(""),
    );
    let items = expand(&table(None, Some("m1-{hardware}"), None), &on(m1.clone())).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &on(m1), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
    assert_eq!(
        rows[0].revert.as_deref(),
        Some("/usr/sbin/scutil --set LocalHostName m1-Air-M2-24GB-1TB-2")
    );
    let m4 = HostFacts {
        model_name: "MacBook Pro".into(),
        chip: "Intel Core i7".into(),
        processor_speed: "3.1 GHz".into(),
        memory_bytes: 17_179_869_184,
        disk_bytes: 1_000_555_581_440,
        ..HostFacts::default()
    };
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get ComputerName",
        Ran::ok("m4 Macbook 3.1GHz i7 16GB 1TB\n"),
    )
    .on(NETBIOS_READ, Ran::ok("M4-MACBOOK-3-1G\n"))
    .on(&services_read(), Ran::ok(SERVICES))
    .on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("m4-Macbook-31GHz-i7-16GB-1TB\n"),
    );
    let items = expand(&described("m4"), &on(m4.clone())).unwrap();
    let rows = engine::run(&items, Mode::Plan, &sys, &on(m4), &mut NoJournal);
    let to: Vec<&str> = rows.iter().map(|r| r.to.as_deref().unwrap()).collect();
    assert_eq!(
        to,
        [
            "\"m4 MacBook Pro Intel Core i7 3.1GHz 16GB 1TB\"",
            "\"M4-MACBOOK-PRO\"",
            "\"m4-MacBook-Pro-Intel-Core-i7-3-1GHz-16GB-1TB\""
        ]
    );
    for (expected, observed, outcome) in [
        ("m85-dog", "m85-dog-2", Outcome::Blocked),
        ("m85-dog", "m85-dog-02", Outcome::Planned),
        ("m85-dog", "m85-dog-1", Outcome::Planned),
        ("m85-dog", "m85-cat-2", Outcome::Planned),
        ("m85-dog", "m85-dog-2-extra", Outcome::Planned),
        ("m85-dog", "m85-dog", Outcome::Ok),
        ("m1-MacBook-Air", "m1-MacBook-Air-2", Outcome::Blocked),
    ] {
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok(observed));
        sys.on("/usr/sbin/scutil --get HostName", Ran::ok(expected));
        let items = expand(&table(None, Some(expected), Some(expected)), &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Plan, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, outcome, "{observed}: {rows:?}");
        assert_eq!(
            rows[1].outcome,
            Outcome::Ok,
            "another root item still runs: {rows:?}"
        );
        assert!(!sys.calls().iter().any(|call| call.contains("--set")));
    }
    for (observed, outcome) in [
        ("m85 Dog (2)", Outcome::Blocked),
        ("m85 Dog (27)", Outcome::Blocked),
        ("m85 Dog (02)", Outcome::Planned),
        ("m85 Dog (1)", Outcome::Planned),
        ("m85 Cat (2)", Outcome::Planned),
        ("m85 Dog (2) extra", Outcome::Planned),
        ("m85 Dog", Outcome::Ok),
    ] {
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok(observed));
        sys.on("/usr/sbin/scutil --get HostName", Ran::ok("m85-dog"));
        let items =
            expand_system_names(&table(Some("m85 Dog"), None, Some("m85-dog")), &ctx()).unwrap();
        let mode = if outcome == Outcome::Blocked {
            Mode::Apply
        } else {
            Mode::Plan
        };
        let rows = engine::run(&items, mode, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, outcome, "{observed}: {rows:?}");
        assert_eq!(rows[1].outcome, Outcome::Ok, "another item still runs");
        assert!(!sys.calls().iter().any(|call| call.contains("--set")));
    }
}

#[test]
fn a_mac_whose_hardware_could_not_be_read_keeps_its_names_and_is_unknown() {
    let sys = Fake::root();
    let mut facts = air();
    facts.memory_bytes = 0;
    let items = expand(&described("m14"), &on(facts.clone())).unwrap();
    assert_eq!(items.len(), 3);
    let rows = engine::run(&items, Mode::Apply, &sys, &on(facts), &mut NoJournal);
    for r in &rows {
        assert_eq!(r.outcome, Outcome::Unknown, "{r:#?}");
        assert!(r.why.as_deref().unwrap().contains("hw.memsize"), "{r:#?}");
    }
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    let err = expand(&table(None, Some("{tga}-{hardware}"), None), &ctx())
        .err()
        .unwrap();
    assert!(err.contains("placeholder"), "{err}");
}

#[test]
fn a_local_hostname_too_long_for_a_label_drops_hardware_from_the_right_never_the_tag() {
    let m10 = HostFacts {
        model_name: "MacBook Pro".into(),
        chip: "Apple M4 Max".into(),
        memory_bytes: 38_654_705_664,
        disk_bytes: 1_000_555_581_440,
        ..HostFacts::default()
    };
    let local = |tag: &str| {
        let items = expand(
            &table(None, Some(&format!("{tag}-{{hardware}}")), None),
            &on(m10.clone()),
        )?;
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"));
        match items[0].probe(&sys, &on(m10.clone())) {
            Probe::Change { to, .. } => Ok(to.trim_matches('"').to_owned()),
            other => Err(format!("{other:?}")),
        }
    };
    assert_eq!(local("m10").unwrap(), "m10-MacBook-Pro-M4-Max-36GB-1TB");
    let tag40 = "classroom-b-bench-twelve-left-by-the-door";
    assert_eq!(tag40.len(), 41);
    assert_eq!(local(tag40).unwrap(), format!("{tag40}-MacBook-Pro-M4-Max"));
    let tag60 = "a".repeat(60);
    assert_eq!(local(&tag60).unwrap(), tag60);
    assert!(local(&"a".repeat(64)).is_err());
}

#[test]
fn the_tag_is_the_leading_letters_and_digits_or_the_whole_name() {
    for (name, tagged) in [
        ("m10-MacBook-M4-Max-36GB-1TB", "m10"),
        ("m1-Air-M2-24GB-1TB-2", "m1"),
        ("m14-Air-M5-32GB-4TB-prime", "m14"),
        ("m100-macbook-M4-Pro-48GB-500GB", "m100"),
        ("m10", "m10"),
        ("lab07", "lab07"),
        ("lab-07", "lab-07"),
        ("spark-4d0f-2", "spark-4d0f-2"),
        ("m10abc-x", "m10abc-x"),
        ("10-x", "10-x"),
        ("Manageds-Virtual-Machine", "Manageds-Virtual-Machine"),
        ("", ""),
    ] {
        assert_eq!(tag(name), tagged, "{name}");
    }
}

/// The Computer Name and Local hostname `[names]` asks for on a Mac with `facts`, its
/// `{tag}` filled with `tag` as the controller (or `here`) fills it.
fn names_for(tag: &str, facts: &HostFacts) -> (String, String) {
    let items = expand(&described(tag), &on(facts.clone())).unwrap();
    let sys = Fake::root();
    sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok("x\n"))
        .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("x\n"))
        .on(NETBIOS_READ, Ran::ok("X\n"))
        .on(&services_read(), Ran::ok(SERVICES));
    let to: Vec<String> = items
        .iter()
        .filter(|i| i.id() != "names.netbios-name")
        .map(|i| match i.probe(&sys, &on(facts.clone())) {
            Probe::Change { to, .. } => to.trim_matches('"').to_owned(),
            other => panic!("{other:?}"),
        })
        .collect();
    (to[0].clone(), to[1].clone())
}

#[test]
fn a_name_that_already_describes_the_mac_is_not_described_again() {
    let vm = HostFacts {
        model_name: "Virtual Machine".into(),
        chip: "Apple M5 (Virtual)".into(),
        memory_bytes: 8_589_934_592,
        disk_bytes: 90_000_003_072,
        ..HostFacts::default()
    };
    let m10 = HostFacts {
        model_name: "MacBook Pro".into(),
        chip: "Apple M4 Max".into(),
        memory_bytes: 38_654_705_664,
        disk_bytes: 1_000_555_581_440,
        ..HostFacts::default()
    };
    let named = (
        "lab-03 Virtual Machine M5 8GB 90GB".to_owned(),
        "lab-03-Virtual-Machine-M5-8GB-90GB".to_owned(),
    );
    assert_eq!(names_for("lab-03", &vm), named);
    assert_eq!(names_for(&named.1, &vm), named);
    assert_eq!(
        names_for(&format!("{}-Virtual-Machine-M5", named.1), &vm),
        named
    );
    assert_eq!(
        names_for("LAB-03-virtual-machine-m5-8gb-90gb", &vm).1,
        "LAB-03-Virtual-Machine-M5-8GB-90GB"
    );
    let cut = "classroom-b-bench-twelve-left-by-the-door-MacBook-Pro-M4-Max";
    assert_eq!(names_for(cut, &m10).1, cut);
    assert_eq!(
        names_for("Andrews-MacBook-Pro", &m10),
        (
            "Andrews MacBook Pro M4 Max 36GB 1TB".to_owned(),
            "Andrews-MacBook-Pro-M4-Max-36GB-1TB".to_owned()
        )
    );
    assert_eq!(
        names_for("lab-MacBook-Pro-1", &m10).1,
        "lab-MacBook-Pro-1-MacBook-Pro-M4-Max-36GB-1TB"
    );
    assert_eq!(
        names_for("MacBook-Pro", &m10).1,
        "MacBook-Pro-MacBook-Pro-M4-Max-36GB-1TB"
    );
    assert_eq!(names_for("m10", &m10).1, "m10-MacBook-Pro-M4-Max-36GB-1TB");
    let computer = |template: &str| {
        let t = table(Some(template), None, None);
        let item = &expand(&t, &on(m10.clone())).unwrap()[0];
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get ComputerName", Ran::ok("x\n"));
        match item.probe(&sys, &on(m10.clone())) {
            Probe::Change { to, .. } => to,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        computer("\u{7814}\u{7a76}\u{5ba4} MacBook Pro {hardware}"),
        "\"\u{7814}\u{7a76}\u{5ba4} MacBook Pro M4 Max 36GB 1TB\""
    );
    assert_eq!(
        computer("(MacBook Pro) {hardware}"),
        "\"(MacBook Pro) MacBook Pro M4 Max 36GB 1TB\""
    );
}
