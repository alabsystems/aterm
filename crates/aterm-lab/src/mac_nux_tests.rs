// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn scenarios_parse_by_name_in_run_order_each_once() {
    assert_eq!(
        parse_scenarios("doctor,init-apply,doctor").unwrap(),
        [Scenario::InitApply, Scenario::Doctor]
    );
    assert_eq!(parse_scenarios("all").unwrap(), ALL);
    assert_eq!(parse_scenarios("vault, all").unwrap(), ALL);
    assert!(
        parse_scenarios("init")
            .unwrap_err()
            .contains("no row 'init'")
    );
    assert!(parse_scenarios("").is_err());
    for s in every() {
        assert_eq!(parse_scenarios(s.name()).unwrap(), [s]);
    }
    assert_eq!(
        parse_scenarios("lan-allowed").unwrap(),
        [Scenario::LanAllowed]
    );
    assert!(!ALL.contains(&Scenario::LanAllowed));
    assert!(!FAULTS.contains(&Scenario::LanAllowed));
    // The fault rows come by name or as `faults`, never with `all`, and after its rows.
    assert_eq!(parse_scenarios("faults").unwrap(), FAULTS);
    assert!(
        parse_scenarios("all")
            .unwrap()
            .iter()
            .all(|s| !FAULTS.contains(s))
    );
    assert_eq!(
        parse_scenarios("dmg,doctor").unwrap(),
        [Scenario::Doctor, Scenario::Dmg]
    );
    let names: Vec<_> = every().map(Scenario::name).collect();
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "{names:?}");
    // Only kill-publish is refused before a guest; every fault row runs the app.
    assert_eq!(
        every()
            .filter(|s| s.not_run().is_some())
            .collect::<Vec<_>>(),
        [Scenario::KillPublish]
    );
    assert!(FAULTS.iter().all(|s| s.needs_app()));
    // The tree rows come by name or as `tree`, never with `all` or `faults`, last; they alone
    // read a tree (so the person base), and run no app.
    assert_eq!(parse_scenarios("tree").unwrap(), TREE);
    assert_eq!(
        parse_scenarios("reveal-tab-control,doctor,kill-stage").unwrap(),
        [
            Scenario::Doctor,
            Scenario::KillStage,
            Scenario::RevealTabControl
        ]
    );
    assert!(
        parse_scenarios("all,faults")
            .unwrap()
            .iter()
            .all(|s| !s.reads_tree())
    );
    assert_eq!(every().filter(|s| s.reads_tree()).collect::<Vec<_>>(), TREE);
    assert!(TREE.iter().all(|s| !s.needs_app() && !s.grows_disk()));
    assert!(every().all(|s| s.display().is_some() == s.reads_tree()));
}

#[test]
fn the_arguments_take_every_flag_and_refuse_a_stray_word() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let ask = parse(&args(&[]), "img".into()).unwrap();
    assert_eq!(ask.scenarios, ALL);
    assert_eq!(ask.image, "img");
    assert!(!ask.standard_user && !ask.keep);
    assert_eq!((ask.app, ask.record), (None, None));
    let ask = parse(
        &args(&[
            "--app",
            "/x/aterm.app",
            "--scenario",
            "vault,doctor",
            "--standard-user",
            "--keep",
            "--record",
            "runs/n.md",
            "--image",
            "other",
        ]),
        "img".into(),
    )
    .unwrap();
    assert_eq!(ask.app.as_deref(), Some(Path::new("/x/aterm.app")));
    assert_eq!(ask.scenarios, [Scenario::Doctor, Scenario::Vault]);
    assert!(ask.standard_user && ask.keep);
    // A relative path is the caller's, never the lab's own `/`.
    assert_eq!(
        ask.record,
        Some(PathBuf::from(format!(
            "{}/runs/n.md",
            crate::paths::caller_dir().trim_end_matches('/')
        )))
    );
    assert_eq!(ask.image, "other");
    assert_eq!(parse(&args(&["stray"]), "i".into()).unwrap_err().code, 2);
    assert_eq!(
        parse(&args(&["--scenario", "nope"]), "i".into())
            .unwrap_err()
            .code,
        2
    );
    // `lan-allowed` runs alone with sudo asking, on the app built here, nobody at the
    // console. NEGATIVE CONTROLS: each flag or row its base cannot answer for is refused.
    let ask = parse(&args(&["--scenario", "lan-allowed"]), "i".into()).unwrap();
    assert!(ask.sudo_asks && !ask.person && !ask.reader && ask.app.is_none());
    for refused in [
        &["--scenario", "lan-allowed,doctor"][..],
        &["--scenario", "lan-allowed", "--app", "/x/aterm.app"],
        &["--scenario", "lan-allowed", "--standard-user"],
        &["--scenario", "lan-allowed", "--person"],
        &["--scenario", "lan-allowed", "--reader"],
    ] {
        let fail = parse(&args(refused), "i".into()).unwrap_err();
        assert_eq!(fail.code, 2, "{refused:?}");
        let said = fail.msg.unwrap_or_default();
        assert!(said.contains("lan-allowed"), "{refused:?}: {said}");
    }
    // Only the rows that run the app need it built or handed over.
    let needing: Vec<_> = ALL.into_iter().filter(|s| !s.needs_app()).collect();
    assert_eq!(
        needing,
        [
            Scenario::DevtoolsControl,
            Scenario::Successor,
            Scenario::InstallSh
        ]
    );
}

#[test]
fn debug_successor_builds_its_test_image_in_an_explicit_separate_run() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let ask = parse(&args(&["--scenario", "debug-successor"]), "image".into()).unwrap();
    assert_eq!(ask.scenarios, [Scenario::DebugSuccessor]);
    assert!(!ALL.contains(&Scenario::DebugSuccessor));
    for words in [
        &["--scenario", "debug-successor,successor"][..],
        &["--scenario", "debug-successor", "--app", "/x/aterm.app"][..],
    ] {
        assert!(parse(&args(words), "image".into()).is_err());
    }
    assert!(
        AppBuildProfile::Test
            .args()
            .windows(2)
            .any(|pair| pair == ["--profile", "test"])
    );
    assert!(
        AppBuildProfile::Release
            .args()
            .windows(2)
            .any(|pair| pair == ["--profile", "release"])
    );
}

#[test]
fn the_base_flags_pick_a_base_a_count_and_a_dated_record() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let golden = "ghcr.io/cirruslabs/macos-golden-gate-base:latest";
    let ask = parse(&args(&[]), golden.into()).unwrap();
    assert!(!ask.person && !ask.no_clt && !ask.vanilla);
    assert_eq!(ask.repeat, 1);
    let ask = parse(
        &args(&["--person", "--no-clt", "--repeat", "3"]),
        golden.into(),
    )
    .unwrap();
    assert!(ask.person && ask.no_clt && !ask.vanilla && !ask.later);
    assert_eq!((ask.repeat, ask.image.as_str()), (3, golden));
    // Later is the person's answer: it takes a person.
    assert!(
        parse(&args(&["--person", "--later"]), golden.into())
            .unwrap()
            .later
    );
    assert_eq!(
        parse(&args(&["--later"]), golden.into()).unwrap_err().code,
        2
    );
    // The reader reads what the person would answer: one or the other.
    let ask = parse(&args(&["--reader", "--no-clt"]), golden.into()).unwrap();
    assert!(ask.reader && !ask.person && ask.no_clt && !ask.sudo_asks);
    // A sudo that asks is the administrator's: a standard account has none.
    let ask = parse(&args(&["--sudo-asks", "--reader"]), golden.into()).unwrap();
    assert!(ask.sudo_asks && ask.reader);
    assert_eq!(
        parse(&args(&["--sudo-asks", "--standard-user"]), golden.into())
            .unwrap_err()
            .code,
        2
    );
    assert_eq!(
        parse(&args(&["--reader", "--person"]), golden.into())
            .unwrap_err()
            .code,
        2
    );
    // Its cell: the words, never "none" for a reader that could not read.
    let read = crate::mac_nux_guest::Reader {
        trusted: Some(true),
        words: vec![crate::mac_nux_guest::Words {
            at_ms: 1500,
            pid: 80,
            owner: "CoreServicesUIAgent".into(),
            words: "AXStaticText \"x\"".into(),
        }],
    };
    assert_eq!(
        reader_value(&read, 1000).to_string(),
        "CoreServicesUIAgent 500 ms: AXStaticText \"x\""
    );
    let blind = crate::mac_nux_guest::Reader {
        trusted: Some(false),
        words: Vec::new(),
    };
    assert!(matches!(reader_value(&blind, 0), Value::Missing(_)));
    let quiet = crate::mac_nux_guest::Reader {
        trusted: Some(true),
        words: Vec::new(),
    };
    assert_eq!(reader_value(&quiet, 0).to_string(), "none");
    // The vanilla image is the -base image's sibling: the same macOS, no Command Line Tools.
    let ask = parse(&args(&["--vanilla"]), golden.into()).unwrap();
    assert_eq!(
        ask.image,
        "ghcr.io/cirruslabs/macos-golden-gate-vanilla:latest"
    );
    assert_eq!(
        vanilla_image("ghcr.io/cirruslabs/macos-tahoe-base"),
        Some("ghcr.io/cirruslabs/macos-tahoe-vanilla:latest".into())
    );
    assert_eq!(vanilla_image("ghcr.io/x/other:1"), None);
    for refused in [
        &["--vanilla", "--image", "ghcr.io/x/other:1"][..],
        &["--no-clt", "--vanilla"],
        &["--repeat", "0"],
        &["--repeat", "11"],
        &["--repeat", "x"],
    ] {
        assert_eq!(
            parse(&args(refused), golden.into()).unwrap_err().code,
            2,
            "{refused:?}"
        );
    }
    // `--record` alone appends to today's dated table in the lab's state, wherever it
    // stands among the flags.
    let dated = format!(
        "mac-nux/mac-nux-{}.md",
        crate::util::ymd(crate::util::now_secs())
    );
    for alone in [&["--record"][..], &["--record", "--keep"]] {
        let ask = parse(&args(alone), golden.into()).unwrap();
        let file = ask.record.unwrap();
        assert!(file.ends_with(&dated), "{}", file.display());
        assert!(file.starts_with(&paths().state), "{}", file.display());
        assert_eq!(ask.keep, alone.len() == 2);
    }
}

#[test]
fn a_report_names_each_row_left_not_in_place() {
    let json = r#"{"items":[{"id":"workstation.dock","outcome":"changed"},{"id":"base.clt","outcome":"person"},
            {"id":"base.ok","outcome":"ok"},{"id":"secrets.vault","outcome":"failed","why":"no vault answered"},{"id":"x.na","outcome":"not-applicable"},
            {"id":"repos.clone","outcome":"deferred"}]}"#
        .replace('\n', "");
    let j = read_report_json(&json).unwrap();
    assert_eq!(
        j.not_ok,
        [
            ("base.clt".to_owned(), "person".to_owned()),
            ("secrets.vault".to_owned(), "failed".to_owned()),
            ("repos.clone".to_owned(), "deferred".to_owned()),
        ]
    );
    let mut row = Row::new(Scenario::InitApply);
    not_ok_cell(&mut row, "", "init --check --json", Some(&j));
    assert_eq!(
        row.cells[0],
        (
            "rows not ok (init --check --json)".to_owned(),
            Value::Text("base.clt person, secrets.vault failed, repos.clone deferred".into())
        )
    );
    // A failed row names its cause; a row with none adds nothing.
    assert_eq!(
        row.cells[1],
        (
            "why rows not ok (init --check --json)".to_owned(),
            Value::Text("secrets.vault: no vault answered".into())
        )
    );
    assert_eq!(row.cells.len(), 2);
    // NEGATIVE CONTROL: a report every row of which is in place names none, and one that did
    // not parse is not measured, never "none".
    let all_ok =
        read_report_json(r#"{"items":[{"id":"a","outcome":"ok"},{"id":"b","outcome":"changed"}]}"#);
    let mut row = Row::new(Scenario::InitApply);
    not_ok_cell(&mut row, "", "init", all_ok.as_ref());
    not_ok_cell(&mut row, "", "init", None);
    assert_eq!(row.cells[0].1, Value::Text("none".into()));
    assert!(matches!(row.cells[1].1, Value::Missing(_)));
    assert_eq!(row.cells.len(), 2);
}

#[test]
fn xcode_select_and_the_state_folder_read_in_a_few_words() {
    assert_eq!(
        xcode_select_text("/Library/Developer/CommandLineTools\nexit=0\n"),
        "/Library/Developer/CommandLineTools"
    );
    let failed = xcode_select_text(
        "xcode-select: error: unable to get active developer directory, use `sudo xcode-select --switch path/to/Xcode.app` to set one (or see `man xcode-select`)\nexit=2\n",
    );
    assert!(
        failed.starts_with(
            "fails (exit 2: xcode-select: error: unable to get active developer directory"
        ) && failed.ends_with("…)"),
        "{failed}"
    );
    assert_eq!(xcode_select_text(""), "fails (exit ?: )");

    assert_eq!(state_summary("(absent)\n"), "no aterm folder");
    let listing = "./doctor\n./doctor/initial-setup.json\n./doctor/first-run.json\n./pkg\n\
                   == doctor/initial-setup.json\n{\"state\":\"pending\"}\n";
    assert_eq!(
        state_summary(listing),
        "4 entries; doctor/: initial-setup.json, first-run.json"
    );
    assert_eq!(state_summary("./pkg\n"), "1 entries; doctor/: none");
}

/// A12: the guest's packet filter refuses both ALab ports, the vault's and its members'
/// (each as its crate names it), to this Mac and to every address off tart's network, the
/// host's refusal before tart's pass (a `quick` rule ends the match), loopback passed.
#[test]
fn the_guests_packet_filter_refuses_the_vault_and_member_ports_off_tarts_network() {
    assert_eq!(ALAB_MEMBER_SERVICE, "_alab._tcp");
    let lines: Vec<&str> = PF_GUARD.lines().collect();
    assert_eq!(lines.len(), 4 * PF_GUARDED.len(), "{PF_GUARD}");
    for port in PF_GUARDED {
        let at = |verb: &str, to: &str| {
            let rule = format!("{verb} out quick proto tcp from any to {to} port {port}");
            lines
                .iter()
                .position(|line| *line == rule)
                .unwrap_or_else(|| panic!("no rule {rule:?} in {PF_GUARD}"))
        };
        let host = at("block return", "192.168.64.1");
        assert!(host < at("pass", "192.168.64.0/24"), "port {port}");
        assert!(
            at("pass", "127.0.0.0/8") < at("block return", "any"),
            "port {port}"
        );
        assert!(
            at("pass", "192.168.64.0/24") < at("block return", "any"),
            "port {port}"
        );
    }
}

/// A12: the guest's look at ALab's members fails the row on any answer that is not this
/// Mac's own (whose member port the packet filter refuses), and on a browse that did not run.
#[test]
fn the_members_probe_fails_a_row_that_heard_another_mac_answer_alabs_members() {
    let none = Ok(advert::Split::default());
    assert_eq!(member_probe_verdict(&none).unwrap(), "no _alab._tcp advert");
    let this_macs = Ok(advert::Split {
        others: Vec::new(),
        this_macs: vec!["alab-1a2b3c4d heard over the guest's interface 6".into()],
    });
    assert_eq!(
        member_probe_verdict(&this_macs).unwrap(),
        "no _alab._tcp advert but this Mac's own alab-1a2b3c4d heard over the guest's \
         interface 6"
    );
    // NEGATIVE CONTROLS: another Mac's answer, or a browse that did not run, fails the row.
    let another = Ok(advert::Split {
        others: vec!["alab-9f8e7d6c on the guest's interface 1 (m22.local 192.168.1.40)".into()],
        this_macs: Vec::new(),
    });
    assert_eq!(
        member_probe_verdict(&another).unwrap_err(),
        "the guest heard an ALab member answer (1 _alab._tcp advert(s): alab-9f8e7d6c on the \
         guest's interface 1 (m22.local 192.168.1.40))"
    );
    assert_eq!(
        member_probe_verdict(&Err("its transcript is empty".into())).unwrap_err(),
        "the guest's browse of _alab._tcp did not run (its transcript is empty), so no advert \
         is unproven"
    );
}

/// A kept clone's boot guard: the file it writes holds the guard every row loads, whole, and
/// the boot daemon enables the packet filter on that file at load, so a clone booted again is
/// guarded before its aterm reopens. NEGATIVE CONTROL: a daemon loading any other file, or no
/// `RunAtLoad`, fails it.
#[test]
fn a_kept_clone_loads_the_guard_again_at_boot() {
    let script = pf_guard_at_boot();
    let body = |path: &str| {
        let start = script
            .find(&format!("cat > {path} <<'EOF'\n"))
            .map(|at| at + format!("cat > {path} <<'EOF'\n").len())
            .expect("a heredoc for the file");
        let len = script[start..].find("EOF\n").expect("the heredoc's end");
        script[start..start + len].to_owned()
    };
    assert_eq!(
        body(PF_GUARD_BOOT),
        PF_GUARD,
        "the boot file is the guard, whole"
    );
    let daemon = body(PF_GUARD_DAEMON);
    assert!(
        daemon.contains(&format!(
            "<array><string>/sbin/pfctl</string><string>-E</string><string>-f</string>\
             <string>{PF_GUARD_BOOT}</string></array>"
        )),
        "{daemon}"
    );
    assert!(daemon.contains("<key>RunAtLoad</key><true/>"), "{daemon}");
    assert!(
        script.contains(&format!(
            "chown root:wheel {PF_GUARD_BOOT} {PF_GUARD_DAEMON}"
        )),
        "launchd loads only root's daemons: {script}"
    );
    assert!(
        script.ends_with("; /bin/sync"),
        "the files reach the disk before the clone stops: {script}"
    );
}

#[test]
fn the_alab_guard_fails_a_row_that_reached_the_vault_or_heard_a_guest_advert() {
    let none = Ok(advert::Split::default());
    let unreached = alab_probe_verdict("none", "000\n", &none).unwrap();
    assert!(
        unreached.starts_with("Unreached (alab-secrets.local unresolved"),
        "{unreached}"
    );
    // A name that resolves but nothing answers (the pf null route) is still unreached, and
    // so is this Mac's own vault heard over the bridge, whose port pf refuses.
    assert!(alab_probe_verdict("192.168.64.1", "000", &none).is_ok());
    let this_macs = Ok(advert::Split {
        others: Vec::new(),
        this_macs: vec!["alab-secrets heard over the guest's interface 6".into()],
    });
    assert!(alab_probe_verdict("192.168.1.242", "000", &this_macs).is_ok());
    // NEGATIVE CONTROLS: any HTTP answer, any advert that is not this Mac's, or a browse
    // that did not run fails the row.
    let another = Ok(advert::Split {
        others: vec!["nux lab on the guest's interface 1 (nux.local 192.168.64.3)".into()],
        this_macs: Vec::new(),
    });
    assert!(alab_probe_verdict("192.168.64.1", "200", &none).is_err());
    assert!(alab_probe_verdict("none", "403", &none).is_err());
    assert!(alab_probe_verdict("none", "000", &another).is_err());
    assert_eq!(
        alab_probe_verdict("none", "000", &Err("its transcript is empty".into())).unwrap_err(),
        "the guest's browse of _alab-vault._tcp did not run (its transcript is empty), so no \
         advert is unproven"
    );

    let browse = "Browsing for _alab-vault._tcp.local.\n\
                  DATE: ---Mon 26 Oct 2026---\n \
                  8:30:01.123  ...STARTING...\n\
                  Timestamp     A/R    Flags  if Domain               Service Type         Instance Name\n \
                  8:30:01.200  Add        3  15 local.               _alab-vault._tcp.    nux lab\n \
                  8:30:01.201  Add        3   4 local.               _alab-vault._tcp.    alab-secrets\n \
                  8:30:01.202  Add        2   9 local.               _alab-vault._tcp.    thunderbolt\n \
                  8:30:01.203  Rmv        0  15 local.               _alab-vault._tcp.    nux lab\n";
    let name_of = |index: u32| match index {
        15 => Some("bridge100".to_owned()),
        4 => Some("en0".to_owned()),
        9 => Some("bridge0".to_owned()),
        _ => None,
    };
    // The office's own vault on en0 and a Thunderbolt bridge are not on tart's bridge.
    let on_bridge = bridge_heard(browse, &name_of);
    assert_eq!(on_bridge.len(), 1, "{on_bridge:?}");
    assert_eq!(
        (on_bridge[0].instance.as_str(), on_bridge[0].interface),
        ("nux lab", 15)
    );
    assert!(bridge_heard(&browse.replace("15 local.", "77 local."), &name_of).is_empty());
}

/// A browse counts only once its transcript shows it ran: a quiet one (no header, as `dns-sd`
/// prints the header with its first answer) is zero adverts, while an empty transcript (a
/// `script` killed before it flushed) or a failed call is no count at all.
#[test]
fn a_browse_that_did_not_run_proves_no_advert_absent() {
    // As `script -q -t 0` wrote them on this Mac (2026-10-28): its `^D^H^H` and `\r`s kept.
    let quiet = "\u{4}\u{8}\u{8}Browsing for _nux-none-t0._tcp.local.\r\n\
                 DATE: ---Wed 28 Oct 2026---\r\n\
                 20:23:54.199  ...STARTING...\r\n";
    assert_eq!(browse_adds(quiet), Ok(Vec::new()));
    let heard = format!(
        "{quiet}Timestamp     A/R    Flags  if Domain               Service Type         Instance Name\r\n\
         20:23:54.300  Add        3  15 local.               _alab._tcp.          nux lab\r\n\
         20:23:54.301  Rmv        0  15 local.               _alab._tcp.          nux lab\r\n"
    );
    assert_eq!(browse_adds(&heard).map(|h| h.len()), Ok(1));
    assert_eq!(browse_ran(&heard), Ok(()));

    // NEGATIVE CONTROLS: what a browse that never looked leaves reads as unread, never 0.
    assert_eq!(browse_adds(""), Err("its transcript is empty".to_owned()));
    assert_eq!(
        browse_adds("\u{4}\u{8}\u{8}\r\n"),
        Err("its transcript is empty".to_owned())
    );
    assert_eq!(
        browse_adds("Browsing for _alab._tcp.local.\r\n"),
        Err("no browse started: Browsing for _alab._tcp.local.".to_owned())
    );
    assert_eq!(
        browse_adds("DNSService call failed -65563\r\n"),
        Err("dns-sd failed: DNSService call failed -65563".to_owned())
    );
    let errored = format!("{quiet}20:23:55.000  Error code -65537\r\n");
    assert_eq!(
        browse_adds(&errored),
        Err("dns-sd failed: 20:23:55.000  Error code -65537".to_owned())
    );
}

#[test]
fn the_local_network_asker_reads_from_the_unified_log_and_ps() {
    let blocks = [
        r#"{"timestamp":"2026-10-28 14:18:24.716000+0000","eventMessage":"Got local network blocked notification: pid: 777, uuid: 758D74AB-18EA-3966-BDA9-9DB45651C793, bundle_id: (null)"}"#,
        r#"{"timestamp":"2026-10-28 14:18:24.716500+0000","eventMessage":"LocalNetwork: found bundle id com.aterm.aterm by PID"}"#,
    ]
    .join("\n");
    assert_eq!(
        local_network_blocks(&blocks),
        [(Some(1_793_197_104_716), 777)]
    );
    let ps = "  777     1 /Applications/aterm.app/Contents/MacOS/aterm\n  827   777 /Applications/aterm.app/Contents/MacOS/aterm acaffeine serve\n";
    let tree = lan::ps_tree(ps);
    assert_eq!(
        tree.get(&777).map(|(_, c)| c.as_str()),
        Some("/Applications/aterm.app/Contents/MacOS/aterm")
    );
    assert_eq!(tree.len(), 2);
    assert_eq!(lan::whose(&tree, 777), lan::Whose::Aterm);
}

#[test]
fn a_folder_dialog_counts_for_aterm_after_the_mark_only() {
    assert_eq!(
        log_timestamp_ms("2026-10-26 08:30:01.123456-0700"),
        Some(1_793_028_601_123)
    );
    assert_eq!(
        log_timestamp_ms("2026-10-26 15:30:01.1+0000"),
        Some(1_793_028_601_100)
    );
    assert_eq!(
        log_timestamp_ms("2026-10-26 15:30:01+0000"),
        Some(1_793_028_601_000)
    );
    assert_eq!(log_timestamp_ms("yesterday"), None);

    let log = [
        r#"{"timestamp":"2026-10-26 08:30:00.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.1, service=kTCCServiceSystemPolicyDocumentsFolder, subject=Sub:{com.aterm.aterm}, "}"#,
        r#"{"timestamp":"2026-10-26 08:30:05.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyDownloadsFolder, subject=Sub:{com.aterm.aterm}, "}"#,
        r#"{"timestamp":"2026-10-26 08:30:06.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.3, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{\/usr\/local\/bin\/tart-guest-agent}, "}"#,
        r#"{"timestamp":"2026-10-26 08:30:07.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.4, service=kTCCServiceSystemPolicyAllFiles, subject=Sub:{com.aterm.aterm}, "}"#,
        r#"{"timestamp":"2026-10-26 08:30:08.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.5, service=kTCCServiceAccessibility, subject=Sub:{com.aterm.aterm}, "}"#,
        r#"{"eventMessage":"AUTHREQ_CTX: msgID=1.6, service=kTCCServiceSystemPolicyDocumentsFolder"}"#,
    ]
    .join("\n");
    let prompts = tcc_prompt_events(&log);
    assert_eq!(prompts.len(), 5);
    assert_eq!(prompts[2].subject, "/usr/local/bin/tart-guest-agent");
    assert_eq!(prompts[0].at_ms, Some(1_793_028_600_000));
    // Before the walk granted Full Disk Access (the mark) the Documents dialog came; after it
    // only the Downloads one, never the guest agent's Desktop or Full Disk Access itself.
    assert_eq!(folder_prompts_after(&prompts, 0), 2);
    assert_eq!(folder_prompts_after(&prompts, 1_793_028_601_000), 1);
    assert_eq!(folder_prompts_after(&prompts, 1_793_028_609_000), 0);
}

#[test]
fn the_helper_is_found_in_cargos_json_and_free_space_in_df() {
    let json = r#"{"reason":"compiler-artifact","target":{"name":"aterm-lab","kind":["bin"]},"executable":"/c/debug/aterm-lab"}
{"reason":"compiler-artifact","target":{"name":"aterm-lab-guest","kind":["bin"]},"executable":"/c/debug/aterm-lab-guest"}
{"reason":"build-finished","success":true}"#;
    assert_eq!(
        bin_executable(json, "aterm-lab-guest"),
        Some(PathBuf::from("/c/debug/aterm-lab-guest"))
    );
    assert_eq!(bin_executable(json, "aterm"), None);
    assert_eq!(bin_executable("", "aterm-lab-guest"), None);

    let df = "Filesystem   1G-blocks Used Available Capacity iused ifree %iused  Mounted on\n\
              /dev/disk3s5       926  833        67    93%  3.4M  698M    0%   /System/Volumes/Data\n";
    assert_eq!(df_free_gb(df), Some(67));
    assert_eq!(df_free_gb("df: /nope: No such file or directory\n"), None);
}

#[test]
fn a_drivers_record_reads_back_whole() {
    let record = "start_epoch_ms 1792950876218\nfirst_byte_ms 435\n\
                  prompt 8550 7000 Secrets vault URL or path (Enter keeps http://x, none skips):\n\
                  prompt 8551 1 Vault join password\nend_ms 18197\nprompts 2\n\
                  quiet_waited_ms 1500\nexit 1\n";
    let d = read_drive(record);
    assert_eq!(d.start_epoch_ms, Some(1_792_950_876_218));
    assert_eq!(d.first_byte_ms, Some(435));
    assert_eq!(d.end_ms, Some(18197));
    assert_eq!(d.prompts.len(), 2);
    assert_eq!(d.prompts[0].1, 7000);
    assert_eq!(d.prompts[1], (8551, 1, "Vault join password".to_owned()));
    assert_eq!(d.quiet_waited_ms, Some(1500));
    assert_eq!(d.exit, Some(1));
    assert_eq!(d.hang, None);
    let mut row = Row::new(Scenario::InitApply);
    drive_cells(&mut row, "", &d);
    assert_eq!(row.outcome, Outcome::Measured);

    // A hang fails the row; a record cut before its end measures nothing there, never 0.
    let hung = read_drive("start_epoch_ms 1\nfirst_byte_ms none\nhang first_byte\n");
    assert_eq!(hung.first_byte_ms, None);
    let mut row = Row::new(Scenario::InitApply);
    drive_cells(&mut row, "", &hung);
    assert!(matches!(row.outcome, Outcome::Failed(_)));
    let shown = table("", &[row]);
    assert!(shown.contains("first byte NOT MEASURED"), "{shown}");
    assert!(shown.contains("end NOT MEASURED"), "{shown}");
    assert!(!shown.contains("end 0 ms"), "{shown}");
}

/// Lines as aterm writes them (2026-10-25, a macOS 27 guest), the phase line as LAB-1's.
const LOG: &str = "1792950909.704 1322 INFO aterm_gui: GPU rendering on Apple Paravirtual device (Metal)\n\
    1792950919.160 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n\
    1792950920.360 1322 WARN aterm: first-run macOS doctor: macOS workstation profile (apply)\u{FFFD}  Local Network: ok\n\
    1792950943.008 1322 INFO aterm_gui: atpkg update pass finished (the store's record owes this launch a pass): exit=failed (next try in 10 min)\n\
    1792950953.369 1322 INFO aterm_gui: full disk access hand-over (automatic): opened\n\
    1792950960.5 1400 INFO aterm_machines::init: aterm init: 41 rows, exit 1; context 120 ms, questions 3 ms, base 2100 ms, workstation 6400 ms, total 8623 ms\n\
    not a log line\n";

#[test]
fn log_lines_parse_and_phases_come_from_the_last_summary_line() {
    let lines = log_lines(LOG);
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[0].at_ms, 1_792_950_909_704);
    assert_eq!(lines[5].at_ms, 1_792_950_960_500);
    assert_eq!(lines[2].level, "WARN");
    assert_eq!(lines[1].target, "aterm_gui::system_state_host");
    let phases = run_phases(&lines, "aterm init:").unwrap();
    assert_eq!(phases["base"], 2100);
    assert_eq!(phases["total"], 8623);
    assert_eq!(phases.len(), 5);
    let mut row = Row::new(Scenario::InitNotty);
    phase_cells(&mut row, "", LOG, "aterm init:");
    assert_eq!(
        row.cells[0].1,
        Value::Text("base 2100, context 120, questions 3, total 8623, workstation 6400".into())
    );
}

#[test]
fn a_log_says_each_message_once_and_nothing_log_show_adds() {
    let log = "{\"eventMessage\":\"Displaying alert for pid 812\",\"timestamp\":\"x\"}\n\
               {\"eventMessage\":\"Displaying alert for pid 812\"}\n\
               {\"eventMessage\":\"  \"}\n\
               {\"eventMessage\":\"Alert dismissed\"}\n\
               {\"count\":4,\"finished\":1}\n\
               Filtering the log data\n\
               {\"eventMessage\":\"third\"}\n";
    assert_eq!(
        distinct_messages(log, 6),
        ["Displaying alert for pid 812", "Alert dismissed", "third"]
    );
    assert_eq!(distinct_messages(log, 1), ["Displaying alert for pid 812"]);
    // NEGATIVE CONTROL: a log of nothing but `log show`'s own lines says nothing.
    assert!(distinct_messages("{\"count\":0,\"finished\":1}\nFiltering\n", 6).is_empty());
}

#[test]
fn an_alert_names_the_last_lines_logged_just_before_it() {
    let lines = log_lines(LOG);
    let last = lines[5].at_ms;
    // At the last line: it and those within the window, at most `max`, oldest first.
    let before = lines_before(&lines, last, 60_000, 2);
    assert_eq!(before.len(), 2);
    assert_eq!(before[1].at_ms, last);
    assert!(before[0].at_ms <= before[1].at_ms);
    // NEGATIVE CONTROL: a line after the alert, or older than the window, is never named.
    assert!(lines_before(&lines, lines[0].at_ms - 1, 60_000, 4).is_empty());
    assert!(
        lines_before(&lines, last + 10_000, 1000, 4).is_empty(),
        "a line older than the window is not the alert's"
    );
}

#[test]
fn a_runs_phases_come_from_the_line_it_appended_never_an_earlier_runs() {
    let mark = LOG.lines().count();
    // NEGATIVE CONTROL: a run that wrote no summary line reads as NOT MEASURED, not as the
    // line the run before it left.
    let mut row = Row::new(Scenario::InitApply);
    phase_cells(&mut row, "check: ", &appended(LOG, mark), "aterm init:");
    assert_eq!(row.cells[0].0, "check: phases");
    assert!(matches!(row.cells[0].1, Value::Missing(_)));
    let grown = format!(
        "{LOG}1792950970.000 1400 INFO aterm: aterm init: 39 rows, exit 0; context 5 ms, total 9 ms\n"
    );
    let mut row = Row::new(Scenario::InitApply);
    phase_cells(&mut row, "no-op: ", &appended(&grown, mark), "aterm init:");
    assert_eq!(row.cells[0].0, "no-op: phases (ms)");
    assert_eq!(row.cells[0].1, Value::Text("context 5, total 9".into()));
    // A log that rotated meanwhile (fewer lines than the mark) reads as empty.
    assert_eq!(appended("1.0 1 INFO aterm: x\n", mark), "");
}

#[test]
fn a_log_without_the_line_is_not_measured_never_zero() {
    // NEGATIVE CONTROL: the same log without its summary line (a build before LAB-1).
    let without: String = LOG
        .lines()
        .filter(|l| !l.contains("aterm init:"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(run_phases(&log_lines(&without), "aterm init:"), None);
    let mut row = Row::new(Scenario::InitNotty);
    phase_cells(&mut row, "", &without, "aterm init:");
    assert!(matches!(row.cells[0].1, Value::Missing(_)));
    let shown = table("", &[row]);
    assert!(shown.contains("phases NOT MEASURED"), "{shown}");
    assert!(!shown.contains(" 0"), "{shown}");
    // A doctor run's line is not init's.
    assert_eq!(
        run_phases(
            &log_lines("1.0 1 INFO aterm: aterm doctor: 3 rows, exit 0; gather 5 ms, total 5 ms\n"),
            "aterm init:"
        ),
        None
    );
    // A launch with no such lines measures none of them.
    let launch = read_launch(
        "1792950909.704 1322 INFO aterm_gui: GPU\n",
        1_792_950_909_000,
    );
    assert_eq!(launch.system_state_ms, None);
    assert_eq!(launch.toolset, None);
    assert_eq!(launch.first_log_ms, Some(704));
}

#[test]
fn first_launch_plan_ignores_an_observation_that_did_not_start() {
    let log = "1792950909.704 1322 INFO aterm_gui::system_state_host: system state: the observation did not start\n";
    let launch = read_launch(log, 1_792_950_909_000);
    assert_eq!(launch.first_log_ms, Some(704));
    assert_eq!(launch.system_state_ms, None);
    // The diagnostic remains part of the setup timeline, without becoming a submission.
    assert_eq!(launch.setup.len(), 1);
}

#[test]
fn first_launch_plan_measures_an_actual_submission() {
    let log = "1792950919.160 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n";
    let launch = read_launch(log, 1_792_950_909_538);
    assert_eq!(launch.system_state_ms, Some(9622));
    assert_eq!(launch.fda_granted_ms, None);
}

#[test]
fn first_launch_plan_uses_the_first_submission_after_open_despite_other_setup_lines() {
    let log = "1792950908.900 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n\
        1792950909.704 1322 INFO aterm_gui::system_state_host: system state: the observation did not start\n\
        1792950911.000 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n\
        1792950913.000 1322 INFO aterm_gui::system_state_host: system state: the look for a renewed GitHub token did not start\n\
        1792950919.160 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n";
    let launch = read_launch(log, 1_792_950_909_000);
    assert_eq!(launch.system_state_ms, Some(2000));
    assert_eq!(
        launch.setup.iter().map(|(ms, _)| *ms).collect::<Vec<_>>(),
        [704, 2000, 4000, 10160]
    );
}

#[test]
fn an_elevation_is_its_sudo_once_and_its_root_half_once_and_the_watch_is_neither() {
    let watch = "1000 501 1 admin /bin/sh -c while :; do case \"$c\" in *\"machines here apply\"*) ...\n\
                 1001 812 700 root /usr/bin/sudo -k -p Mac password:  /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1001 813 501 admin ps -axww -o pid=,ppid=,user=,command= machines here apply\n\
                 1002 812 700 root /usr/bin/sudo -k -p Mac password:  /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1002 815 812 root /usr/bin/sudo -k -p Mac password:  /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1002 814 815 root /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1003 814 815 root /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1003 900 700 admin /Applications/aterm.app/Contents/MacOS/aterm ctl ls\n";
    let seen = read_elevations(watch);
    // sudo 815 is 812's use_pty monitor (round 8's first run counted it as a second ask).
    assert_eq!(seen.asks, [(1001, 812)]);
    assert_eq!(seen.root_runs, [(1002, 814)]);
    assert_eq!(sightings(&seen.asks, 1_000_500), "1: at 0 s");
    // Negative control: a second sudo under no sudo (the password asked again) is a second
    // ask, which the row fails on; a root half the account ran as itself is no root run.
    let again = format!(
        "{watch}1300 950 700 root /usr/bin/sudo -k -p Mac password:  /x/aterm machines here apply --plain\n\
         1301 951 950 admin /x/aterm machines here apply --plain\n"
    );
    let seen = read_elevations(&again);
    assert_eq!(seen.asks, [(1001, 812), (1300, 950)]);
    assert_eq!(seen.root_runs, [(1002, 814)]);
    assert_eq!(
        read_elevations("garbage\n1 x 1 root sudo machines here apply\n"),
        Elevations::default()
    );
}

/// The LAN broker is a root aterm too: it is never a root half or an ask, by its argv in the
/// watch and by launchd's count of its ended processes in process accounting.
#[test]
fn the_lan_broker_is_never_counted_as_an_elevation() {
    let watch = "1001 812 700 root /usr/bin/sudo -k -p Mac password:  /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1002 814 812 root /Applications/aterm.app/Contents/MacOS/aterm machines here apply --plain\n\
                 1005 900 1 root /Library/Application Support/aterm/lan/aterm.app/Contents/MacOS/aterm lan serve --source /Applications/aterm.app machines here apply\n";
    let seen = read_elevations(watch);
    assert_eq!(seen.asks, [(1001, 812)]);
    assert_eq!(
        seen.root_runs,
        [(1002, 814)],
        "the broker's argv is no root half"
    );
    // Accounting: the root half, and one broker that ended on its adoption's restart.
    let report = "aterm      -       root     __         0.02 secs Wed Nov  4 11:47 (0:00:00.31)\n\
                  aterm      -       root     __         0.40 secs Wed Nov  4 11:47 (0:00:41.00)\n";
    let restarted = lan::read_launchctl(
        "system/systems.alab.aterm-lan = {\n\tstate = running\n\truns = 2\n\tpid = 950\n}\n",
    );
    assert_eq!(root_halves(report, None, restarted.as_ref()), 1);
    assert_eq!(with_accounting(seen, 1).asks.len(), 1, "one ask, not two");
    // NEGATIVE CONTROL: with no broker the same records are two root halves, two asks.
    assert_eq!(root_halves(report, None, None), 2);
    assert_eq!(with_accounting(Elevations::default(), 2).asks.len(), 2);
    // A broker still running has not ended, so it takes nothing off.
    let running = lan::read_launchctl(
        "system/systems.alab.aterm-lan = {\n\tstate = running\n\truns = 1\n}\n",
    );
    assert_eq!(root_halves(report, None, running.as_ref()), 2);
    // A broker run that had ended before accounting started is in no record: only the
    // runs that ended since come off.
    let earlier = lan::read_launchctl(
        "system/systems.alab.aterm-lan = {\n\tstate = running\n\truns = 2\n\tpid = 900\n}\n",
    );
    assert_eq!(root_halves(report, earlier.as_ref(), restarted.as_ref()), 2);
}

/// A root half too quick for the watch is still counted from process accounting, with its
/// ask; one the watch saw is not counted twice; only `aterm` run as root counts.
#[test]
fn a_quick_elevation_counts_from_process_accounting_once() {
    let report = "sudo       -S      admin    __         0.00 secs Wed Nov  4 11:47 (0:00:00.00)\n\
                  aterm      -       root     __         0.02 secs Wed Nov  4 11:47 (0:00:00.31)\n\
                  aterm      -       admin    __         0.10 secs Wed Nov  4 11:47 (0:00:02.00)\n\
                  DevToolsSecuri -   root     __         0.00 secs Wed Nov  4 11:47 (0:00:00.01)\n";
    assert_eq!(root_aterm_runs(report), 1);
    let quick = with_accounting(Elevations::default(), 1);
    assert_eq!(quick.asks, [(0, 0)]);
    assert_eq!(quick.root_runs, [(0, 0)]);
    assert_eq!(
        sightings(&quick.asks, 1_000_000),
        "1: too quick for the watch (process accounting)"
    );
    let watched = Elevations {
        asks: vec![(1001, 812)],
        root_runs: vec![(1002, 814)],
    };
    let both = with_accounting(watched, 1);
    assert_eq!(
        both.asks,
        [(1001, 812)],
        "the watch's run is the accounted one"
    );
    assert_eq!(both.root_runs, [(1002, 814)]);
    // Negative control: a second root half is a second ask, which the row fails on; a sudo
    // still at its prompt (seen, not yet accounted) stays one.
    assert_eq!(with_accounting(Elevations::default(), 2).asks.len(), 2);
    let waiting = Elevations {
        asks: vec![(1001, 812)],
        root_runs: Vec::new(),
    };
    assert_eq!(with_accounting(waiting, 0).asks, [(1001, 812)]);
}

#[test]
fn the_developer_tools_cell_says_how_aterm_came_onto_the_list_its_password_and_its_switch() {
    let open = 1_000_000;
    let raw = |record: &str, log: &str| {
        devtools_value(
            Some(&devtools_acts(&read_person(record))),
            log,
            open,
            1_400_000,
        )
        .to_string()
    };
    let person = "person 1000000 trusted=yes post=yes password=yes\n";
    let cell = |record: &str, log: &str| raw(&format!("{person}{record}"), log);
    let page = "devtools 1248000 120 pane list=lacks switch=none\n\
                devtools 1248100 120 drag tile=2082 from=1100,300 to=600,420\n\
                click 1254000 120 System Settings devtools-drag\n";
    let taken = "1793243999.000 2082 INFO aterm_gui::devtools_helper: x\n\
                 1254.000 2082 INFO aterm_gui::devtools_helper: devtools: the owner's drag of aterm ended (operation 1: taken)\n";
    // The drop took: a password, aterm on the list, its switch read off and pressed on.
    assert_eq!(
        cell(
            &format!(
                "{page}typed 1258000 120 account 5 Managed_via_Tart unwatched System Settings\n\
                 devtools 1259000 120 pane list=holds switch=off\n\
                 click 1265000 120 System Settings devtools-switch\n\
                 devtools 1265500 120 pane list=holds switch=on\n"
            ),
            taken
        ),
        "aterm added by drag 259000 ms, 5000 ms after the drag (aterm's log: operation 1: \
         taken); password 258000 ms; switch on 265000 ms; page seen 248000 ms"
    );
    // On the list with its switch on, no password asked.
    assert_eq!(
        cell(
            &format!("{page}devtools 1255000 120 pane list=holds switch=on\n"),
            taken
        ),
        "aterm added by drag 255000 ms, 1000 ms after the drag (aterm's log: operation 1: \
         taken); password not asked; switch on already; page seen 248000 ms"
    );
    // The drop did not take, and + did: the add is the +'s.
    let plus = format!(
        "{page}click 1276000 120 System Settings devtools-plus\n\
         devtools 1277000 120 open-panel 120 System Settings: AXSheet; AXButton \"Open\"\n\
         click 1295000 120 System Settings devtools-goto\n\
         click 1301000 120 System Settings devtools-open\n\
         devtools 1302000 120 pane list=holds switch=on\n"
    );
    let not_taken = "1254.000 2082 INFO aterm_gui::devtools_helper: devtools: the owner's drag of aterm ended (operation 0: not taken)\n";
    assert_eq!(
        cell(&plus, not_taken),
        "aterm added by + 302000 ms, 26000 ms after the +; password not asked; switch on \
         already; page seen 248000 ms"
    );
    // Negative controls. Never on the list: each try, its steps and why it stopped; a drag
    // aterm's log never ended.
    assert_eq!(
        cell(
            &format!(
                "{page}click 1276000 120 System Settings devtools-plus\n\
                 devtools 1296000 120 open-panel not seen within 20 s of +\n"
            ),
            ""
        ),
        "aterm not on the list in the 400 s watched: drag pressed 254000 ms (aterm's log: \
         NOT MEASURED, no line of it from the page on), + pressed 276000 ms; open-panel not \
         seen within 20 s of +; password not asked; page seen 248000 ms"
    );
    // aterm's log read from the page on, with no drag's end in it.
    assert_eq!(
        cell(
            page,
            "1793243999.000 2082 INFO aterm_gui::devtools_helper: x\n"
        ),
        "aterm not on the list in the 400 s watched: drag pressed 254000 ms (aterm's log: no \
         drag ended); password not asked; page seen 248000 ms"
    );
    // A list that read as holding aterm once (an open panel's file view, a sheet closing) and
    // lacked it again: no add.
    assert_eq!(
        cell(
            &format!(
                "{page}devtools 1255000 120 pane list=holds switch=off\n\
                 devtools 1255250 120 pane list=lacks switch=none\n"
            ),
            taken
        ),
        "aterm not on the list in the 400 s watched: drag pressed 254000 ms (aterm's log: \
         operation 1: taken); password not asked; page seen 248000 ms"
    );
    // On the list later with no add of the person's, its switch on already.
    assert_eq!(
        cell(
            "devtools 1248000 120 pane list=lacks switch=none\n\
             devtools 1250000 120 pane list=holds switch=on\n",
            ""
        ),
        "aterm on the list 250000 ms, with no add by the person; switch on already; page seen \
         248000 ms"
    );
    // Added with + alone, no tile to drag; its switch read off, then pressed and read off.
    let plus_only = "devtools 1248000 120 pane list=lacks switch=none\n\
                     devtools 1268000 120 no-tile: no window of the app (1 running) shows the \
                     helper panel's tile\n\
                     click 1268100 120 System Settings devtools-plus\n\
                     click 1275000 120 System Settings devtools-goto\n\
                     click 1276000 120 System Settings devtools-open\n\
                     devtools 1277000 120 pane list=holds switch=off\n";
    assert_eq!(
        cell(plus_only, ""),
        "aterm added by + 277000 ms, 8900 ms after the +; no drag: no window of the app (1 \
         running) shows the helper panel's tile; password not asked; switch off, not pressed; \
         page seen 248000 ms"
    );
    assert_eq!(
        cell(
            &format!("{plus_only}click 1280000 120 System Settings devtools-switch\n"),
            ""
        ),
        "aterm added by + 277000 ms, 8900 ms after the +; no drag: no window of the app (1 \
         running) shows the helper panel's tile; password not asked; switch on 280000 ms \
         (then read off); page seen 248000 ms"
    );
    // A try that stopped: its steps by name; a password dialog read and not typed into; a
    // password typed on another page long after is not this page's.
    let stopped = "devtools 1248000 120 pane list=lacks switch=none\n\
                   click 1268100 120 System Settings devtools-plus\n\
                   skip 1270000 120 System Settings: the administrator's password not typed\n\
                   click 1275000 120 System Settings devtools-goto\n\
                   devtools 1295000 120 skip: the open panel's Open stayed off after the \
                   path's Return\n\
                   click 1295100 120 System Settings devtools-cancel\n\
                   typed 1390000 120 account 5 Managed_via_Tart unwatched System Settings\n";
    assert_eq!(
        cell(stopped, ""),
        "aterm not on the list in the 400 s watched: + pressed 268100 ms, the path's Return \
         275000 ms, Cancel 295100 ms; no drag; skip: the open panel's Open stayed off after \
         the path's Return; password asked 270000 ms, not typed (the focus read a text field \
         not secure); page seen 248000 ms"
    );
    assert_eq!(
        cell(
            "devtools 1248000 120 pane list=lacks switch=none\n\
             click 1268100 120 System Settings devtools-plus\n\
             dialog 1269000 120 System Settings: AXSheet; AXTextField/AXSecureTextField \
             (secure)\n",
            ""
        ),
        "aterm not on the list in the 400 s watched: + pressed 268100 ms; no drag; password \
         asked 269000 ms, not typed; page seen 248000 ms"
    );
    // A person with no password to type reads no password dialog: never "not asked".
    assert_eq!(
        raw(
            &format!("person 1000000 trusted=yes post=yes password=no\n{plus_only}"),
            ""
        ),
        "aterm added by + 277000 ms, 8900 ms after the +; no drag: no window of the app (1 \
         running) shows the helper panel's tile; password NOT MEASURED (the person had none \
         to type); switch off, not pressed; page seen 248000 ms"
    );
    // A person without Accessibility, or with no grants line, read nothing.
    assert_eq!(
        raw("person 1000000 trusted=no post=yes password=yes\n", ""),
        "NOT MEASURED (the person had no Accessibility grant, so it read no window)"
    );
    assert_eq!(
        raw(plus_only, ""),
        "NOT MEASURED (the person's record says none of its grants)"
    );
    assert_eq!(
        cell("devtools 1248000 120 pane list=holds switch=on\n", taken),
        "aterm on the list when the page came; switch on already; page seen 248000 ms",
        "on the list before any add: no add, no password said"
    );
    assert_eq!(
        cell(
            "devtools 1248000 120 pane list=unread switch=none: AXGroup\n",
            ""
        ),
        "NOT MEASURED (the page's list did not read (it read AXGroup); page seen 248000 ms)",
        "judged from the last state"
    );
    assert_eq!(
        cell(
            "devtools 1248000 120 pane list=unread switch=none: AXGroup\n\
             devtools 1249000 120 pane list=lacks switch=none\n",
            ""
        ),
        "aterm not added in the 400 s watched; page seen 248000 ms",
        "a list that read in the end"
    );
    assert_eq!(
        cell(
            "devtools 1248000 120 pane list=lacks switch=none\n\
             devtools 1248100 120 skip: no --app, so the person knows no aterm to add\n",
            ""
        ),
        "aterm not added in the 400 s watched; skip: no --app, so the person knows no aterm \
         to add; page seen 248000 ms"
    );
    // Another page's switch and the Full Disk Access password before the page are not it.
    assert_eq!(
        cell(
            "click 1100000 120 System Settings switch:aterm\n\
             typed 1120000 120 account 5 Managed_via_Tart unwatched System Settings\n",
            taken
        ),
        "not seen in the 400 s watched"
    );
    assert_eq!(
        devtools_value(None, "", open, 1_400_000).to_string(),
        "NOT MEASURED (no person at the console (--person puts one there))"
    );
}

#[test]
fn a_quit_and_reopen_reads_as_the_press_the_quit_the_reopen_and_the_new_pids_first_line() {
    let open = 1_000_000;
    let procs = read_procs(
        "procs 999000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm\n\
         up 1000400 793 start=1000300\n\
         down 1092400 793\n\
         up 1092600 1201 start=1092550\n",
    );
    let log = "1000.500 793 INFO aterm_gui: system state: submitting a plan (Automatic)\n\
               1092.300 793 INFO aterm_gui: quit & reopen: pid 1199 opens /Applications/aterm.app once this process is gone\n\
               1093.100 1201 INFO aterm_gui: permission walk: Quit & Reopen left fda; they wait for the walk's start\n";
    let presses = |sheet: &[(u64, &str)], quit: &[u64]| ReopenPresses {
        sheet: sheet.iter().map(|(at, w)| (*at, (*w).to_owned())).collect(),
        quit: quit.to_vec(),
    };
    // The person's Quit & Reopen, then Quit on aterm's question 6.31 s later (the person waits
    // its quiet between two inputs), and aterm gone 90 ms after that.
    let pressed = presses(&[(1_086_000, "Quit & Reopen")], &[1_092_310]);
    let cells = |presses: Option<&ReopenPresses>, later: bool, procs: &Procs, log: &str| {
        reopen_values(presses, later, procs, log, open, 1_200_000)
            .into_iter()
            .map(|(name, value)| format!("{name} {value}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        cells(Some(&pressed), false, &procs, log),
        [
            "Quit & Reopen sheet Quit & Reopen pressed 86000 ms (the person answers Quit & Reopen)",
            "aterm's quit question Quit pressed 92310 ms, 6310 ms after Quit & Reopen",
            "aterm processes pid 793 300 ms to 92400 ms; pid 1201 92550 ms to the end",
            "aterm quit pid 793 gone 92400 ms, 90 ms after Quit on aterm's question",
            "aterm reopened pid 1201 started 92550 ms, 150 ms after the quit",
            "reopened pid's first log line 93100 ms",
        ]
    );
    // A quit with no question answered before it is timed from the sheet's press.
    assert_eq!(
        cells(
            Some(&presses(&[(1_092_310, "Quit & Reopen")], &[])),
            false,
            &procs,
            log
        )[3],
        "aterm quit pid 793 gone 92400 ms, 90 ms after Quit & Reopen"
    );
    // Negative controls. Later: the app runs on, its question is not answered, and nothing is
    // said to have quit.
    let ran = read_procs("procs 999000 every=50 exe=/A/aterm\nup 1000400 793 start=1000300\n");
    assert_eq!(
        cells(
            Some(&presses(&[(1_092_310, "Later")], &[])),
            true,
            &ran,
            log
        )[1..],
        [
            "aterm's quit question not answered: the person answers it only after its own Quit & Reopen",
            "aterm processes pid 793 300 ms to the end",
            "aterm quit none: pid 793 ran to the end in the 200 s watched",
            "aterm reopened none: nothing quit",
        ]
    );
    // Run 15's shape had the person answered no question: Quit & Reopen pressed, aterm never
    // asked (or asked unseen), and it ran on.
    assert_eq!(
        cells(
            Some(&presses(&[(1_092_310, "Quit & Reopen")], &[])),
            false,
            &ran,
            log
        )[1..4],
        [
            "aterm's quit question not seen in the 200 s watched",
            "aterm processes pid 793 300 ms to the end",
            "aterm quit none: pid 793 ran to the end in the 200 s watched",
        ]
    );
    assert!(
        cells(Some(&presses(&[], &[])), true, &ran, log)[0]
            .ends_with("not seen in the 200 s watched (the person answers Later (--later))")
    );
    // A quit with nothing after it is said, never a reopen; a pid with no log line is NOT
    // MEASURED, never the first process's line.
    let gone = read_procs(
        "procs 999000 every=50 exe=/A/aterm\nup 1000400 793 start=1000300\ndown 1092400 793\n",
    );
    assert_eq!(
        cells(None, false, &gone, log)[4],
        "aterm reopened none: no aterm app process started again in the 200 s watched"
    );
    let unlogged = cells(
        Some(&pressed),
        false,
        &procs,
        &log.lines().take(2).collect::<Vec<_>>().join("\n"),
    );
    assert_eq!(
        unlogged[5],
        "reopened pid's first log line NOT MEASURED (aterm.log has no line from pid 1201 in the 200 s watched)"
    );
    // No person, no watch record, an unreadable process table: NOT MEASURED, never a guess.
    let nobody = cells(None, false, &procs, log);
    for cell in &nobody[..2] {
        assert!(
            cell.contains("NOT MEASURED (no person at the console"),
            "{cell}"
        );
    }
    for (procs, why) in [
        (read_procs(""), "the process watch wrote nothing"),
        (
            read_procs(
                "procs 999000 every=50 exe=/A/aterm\nprocs-error 999050 the kernel's process table did not read\n",
            ),
            "the process watch: the kernel's process table did not read",
        ),
        (
            read_procs("procs 999000 every=50 exe=/A/aterm\n"),
            "the process watch saw no aterm app process in the 200 s watched",
        ),
    ] {
        let said = cells(Some(&pressed), false, &procs, log);
        assert_eq!(said.len(), 5, "{why}");
        for cell in &said[2..] {
            assert!(cell.ends_with(&format!("NOT MEASURED ({why})")), "{cell}");
        }
    }
}

#[test]
fn a_first_launch_reads_each_milestone_after_the_open() {
    let open = 1_792_950_909_538;
    let launch = read_launch(LOG, open);
    assert_eq!(launch.first_log_ms, Some(166));
    assert_eq!(launch.warns, 1);
    assert_eq!(launch.errors, 0);
    assert_eq!(launch.system_state_ms, Some(9622));
    assert_eq!(launch.handovers, [(43831, "opened".to_owned())]);
    assert_eq!(launch.fda_granted_ms, None);
    // Every setup line, in order: the plan, the first-run doctor and the hand-over.
    assert_eq!(
        launch.setup.iter().map(|(ms, _)| *ms).collect::<Vec<_>>(),
        [9622, 10822, 43831]
    );
    assert_eq!(launch.toolset.as_ref().map(|t| t.0), Some(33470));
    assert!(launch.toolset.unwrap().1.contains("exit=failed"));
    let doctor = launch.first_run_doctor.unwrap();
    assert_eq!(doctor.0, 10822);
    // The record's newline, which aterm.log writes as U+FFFD, reads as a space.
    assert!(
        doctor.1.contains("(apply)   Local Network: ok"),
        "{}",
        doctor.1
    );
    // Lines before the open belong to another launch.
    assert_eq!(read_launch(LOG, 1_792_950_960_000).warns, 0);
}

#[test]
fn a_toolset_pass_reads_its_start_and_its_end_after_the_exit() {
    let log = "1792950900.000 1300 INFO aterm_gui: atpkg seed done: an earlier launch's\n\
        1792950901.200 1322 INFO aterm_gui: GPU rendering on Apple Paravirtual device (Metal)\n\
        1792950902.500 1322 INFO aterm_gui: atpkg install pass starting (first run): installing 15 program(s)\n\
        1792950931.000 1322 INFO aterm_gui: atpkg seed done: 15 installed\n\
        1792950940.000 1322 INFO aterm_gui: atpkg update pass finished: exit=ok\n";
    let (start, end) = toolset_pass(log, 1_792_950_901_000);
    assert_eq!(start.map(|s| s.0), Some(1500));
    assert_eq!(end.as_ref().map(|e| e.0), Some(30_000));
    assert!(end.unwrap().1.starts_with("atpkg seed done: 15"));
    // A pass still running has a start and no end; lines before the exit are another run's.
    let running = log.lines().take(3).collect::<Vec<_>>().join("\n");
    assert_eq!(toolset_pass(&running, 1_792_950_901_000).1, None);
    assert_eq!(toolset_pass(log, 1_792_950_950_000), (None, None));
    // Only a failed end sends the row to name the members it left (finding 22).
    assert!(!toolset_failed(
        "atpkg update pass finished: exit=ok outcome=up to date"
    ));
    assert!(!toolset_failed("atpkg seed done: 15 installed"));
    assert!(toolset_failed(
        "atpkg update pass finished (the store's record owes this launch a pass): \
         exit=failed (next try in 10 min) outcome=kimi 2.4.1 installed"
    ));
    assert!(toolset_failed("atpkg install pass failed: exit 1"));
}

#[test]
fn a_prompt_counts_from_an_aterm_tab_after_the_open() {
    let prompts = "1792950905.000 900 none\n1792950911.3212769 1329 aterm\n\
                   1792950912.5 1330 aterm\nbad line\n";
    assert_eq!(first_prompt_ms(prompts, 1_792_950_909.538_574), Some(1783));
    assert_eq!(first_prompt_ms(prompts, 1_792_950_913.0), None);
    assert_eq!(first_prompt_ms("", 0.0), None);
}

#[test]
fn metrics_relaunches_dock_pins_mdns_and_consent_read_from_their_tools() {
    let m = "OK backend=gpu rust_main_to_first_present_ms=1314.64 first_present_ms=1312.79";
    assert_eq!(metric(m, "rust_main_to_first_present_ms"), Some(1314.64));
    assert_eq!(metric(m, "first_present_ms"), Some(1312.79));
    assert_eq!(metric(m, "present_ms"), None);
    assert_eq!(metric("ERR no window", "first_present_ms"), None);
    assert_eq!(first_present(m), Ok(Some(1315)));
    // 0 until the first terminal present: a guest whose window System State fronts.
    let none_yet = "OK backend=gpu frames=3688 rust_main_to_first_present_ms=0.00";
    assert_eq!(first_present(none_yet), Ok(None));
    assert_eq!(first_present("ERR no window"), Err(()));

    let before = "Dock 394\nFinder 401\nSystemUIServer 410\n";
    let after = "Dock 1500\nFinder 401\nSystemUIServer \n";
    assert_eq!(relaunched(before, after), ["Dock", "SystemUIServer"]);
    assert_eq!(relaunched(before, before), Vec::<String>::new());

    let dock = "(\n    {\n        \"tile-data\" = {\n            \"file-label\" = aterm;\n\
                \n            \"file-label\" = \"System Settings\";\n            \"file-label\" = Safari;\n";
    assert_eq!(dock_labels(dock), ["aterm", "System Settings", "Safari"]);

    let tcpdump = "10:00:00 IP 192.168.64.5.5353 > 224.0.0.251.5353: 0 PTR (QM)? _alab-vault._tcp.local. (40)\n\
                   10:00:01 IP 192.168.64.5.5353 > 224.0.0.251.5353: 0 A (QM)? alab-secrets.local. (36)\n\
                   10:00:01 IP 192.168.64.5.5353 > 224.0.0.251.5353: 0 PTR (QM)? _ssh._tcp.local. (32)\n\
                   10:00:02 IP 192.168.64.1.5353 > 224.0.0.251.5353: 0*- [0q] 1/0/0 PTR m106._ssh._tcp.local. (60)\n";
    assert_eq!(alab_questions(tcpdump), 2);
    assert_eq!(alab_questions(""), 0);

    // macOS 27 writes the format string ahead of the message: its `%{public}@` is no service.
    let tcc = r#"{"formatString":"AUTHREQ_PROMPTING: msgID=%{public}@, service=%{public}@, subject=%{public}@, ","eventMessage":"AUTHREQ_PROMPTING: msgID=158.53, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{x}"}
{"eventMessage":"AUTHREQ_PROMPTING: msgID=9.1, service=kTCCServiceAccessibility, subject=Sub:{com.aterm.aterm}"}
{"eventMessage":"AUTHREQ_PROMPTING: msgID=9.2, service=kTCCServiceAccessibility, subject=Sub:{com.aterm.aterm}"}
{"eventMessage":"AUTHREQ_CTX: msgID=9.3, service=kTCCServiceAppleEvents"}"#;
    let seen = tcc_prompts(tcc);
    assert_eq!(seen["Accessibility"], 2);
    assert_eq!(seen["SystemPolicyDesktopFolder"], 1);
    assert_eq!(seen.len(), 2);
}

#[test]
fn a_root_step_is_the_administrators_sudo_and_never_the_standard_accounts_own() {
    let cmd = "pkill -INT -f '^/usr/bin/log stream'";
    assert_eq!(
        root_command(false, cmd),
        r"sudo -n /bin/sh -c 'pkill -INT -f '\''^/usr/bin/log stream'\'''"
    );
    let standard = root_command(true, cmd);
    // The standard account reaches the administrator's sudo over ssh, the whole sudo line one
    // word, so its own quotes survive the second shell.
    let (ssh, word) = standard.split_once(" admin@localhost ").unwrap();
    assert!(ssh.starts_with("ssh -n -o BatchMode=yes"), "{ssh}");
    assert!(ssh.ends_with(&format!("-i ~/{ROOT_KEY}")), "{ssh}");
    assert_eq!(
        crate::util::shell_unquote(word),
        root_command(false, cmd),
        "{standard}"
    );
}

#[test]
fn only_a_standard_accounts_person_has_the_administrator_at_hand() {
    let out = "/var/tmp/aterm-nux/person.txt";
    let app = "/Applications/aterm.app/Contents/MacOS/aterm";
    assert_eq!(
        person_args(out, false, false, app),
        [
            PERSON_HELPER,
            "person",
            out,
            "--password",
            "admin",
            "--app",
            app
        ]
    );
    assert_eq!(
        person_args(out, false, true, app),
        [
            PERSON_HELPER,
            "person",
            out,
            "--password",
            "admin",
            "--sheet",
            "later",
            "--app",
            app
        ]
    );
    assert_eq!(
        person_args(out, true, false, app),
        [
            PERSON_HELPER,
            "person",
            out,
            "--password",
            STANDARD_PASSWORD,
            "--admin",
            "admin:admin",
            "--app",
            app
        ]
    );
    // The cell names whose password went in and the name field, never a password; the
    // account's own password is typed once, its dialog not watched.
    let p = read_person(
        "dialog 900 812 System Settings: AXSheet; AXTextField \"NUX Standard\"\n\
         focus 950 812 name AXTextField \"admin\"\n\
         typed 1000 812 administrator 5 NUX_Standard>admin stayed System Settings\n\
         focus 1200 812 password AXTextField \"admin\"\n\
         skip 1210 812 System Settings: the administrator's password not typed (the focus read \
         a text field not secure)\n\
         typed 1500 812 administrator 5 admin went:640 System Settings\n\
         typed 2000 640 account 13 - unwatched SecurityAgent\n",
    );
    let skipped = p.dialogs.iter().filter(|(_, k, _)| k == "skip").count();
    assert_eq!(
        typed_cell(&p.typed, skipped),
        Value::Text(
            "3: System Settings, the administrator's (name field NUX_Standard>admin), the \
             dialog stayed; System Settings, the administrator's (name field admin), the \
             dialog went 640 ms after Return; SecurityAgent, the account's; 1 try typed \
             nothing (the focus read a text field not secure)"
                .into()
        )
    );
    assert_eq!(
        p.typed.iter().map(|t| t.went).collect::<Vec<_>>(),
        [Went::Stayed, Went::After(640), Went::Unwatched]
    );
    assert_eq!(typed_cell(&[], 0), Value::Count(0));
    assert_eq!(
        p.dialogs,
        [
            (
                900,
                "dialog".to_owned(),
                "System Settings: AXSheet; AXTextField \"NUX Standard\"".to_owned()
            ),
            (
                950,
                "focus".to_owned(),
                "name AXTextField \"admin\"".to_owned()
            ),
            (
                1200,
                "focus".to_owned(),
                "password AXTextField \"admin\"".to_owned()
            ),
            (
                1210,
                "skip".to_owned(),
                "System Settings: the administrator's password not typed (the focus read a \
                 text field not secure)"
                    .to_owned()
            )
        ]
    );
}

#[test]
fn a_release_is_named_by_its_tag_and_a_report_by_its_json() {
    assert_eq!(
        release_tag(
            "location: https://github.com/alabsystems/aterm/releases/download/v0.138.0/aterm-appcast.toml\r\n"
        ),
        Some((0, 138, 0))
    );
    assert_eq!(
        release_tag("location: https://x/download/latest/a.toml"),
        None
    );
    assert_eq!(release_tag("v1.2"), None);
    assert_eq!(release_tag(""), None);

    for outcome in ["ok", "changed"] {
        assert!(completed_root_report(&format!(
            "terminal noise\r\n{{\"complete\":true,\"items\":[{{\"id\":\"base.clt\",\"outcome\":\"{outcome}\"}}]}}\r\n"
        )));
    }
    for report in [
        r#"{"complete":true,"items":[]}"#,
        r#"{"complete":false,"items":[{"id":"base.clt","outcome":"ok"}]}"#,
        r#"{"complete":true,"items":[{"id":"base.clt","outcome":"deferred"}]}"#,
        r#"{"complete":true,"items":[{"id":"base.clt","outcome":"not-applicable"}]}"#,
        r#"{"complete":true,"items":[{"id":"another","outcome":"ok"}]}"#,
        r#"{"complete":true,"items":[{"id":"base.clt"}]}"#,
        r#"{"items":[{"id":"base.clt","outcome":"ok"}]}"#,
        "no report",
    ] {
        assert!(!completed_root_report(report), "{report}");
    }

    let clt = r#"{"id":"base.clt","outcome":"ok"}"#;
    let steps = r#"{"id":"base.clt.root-steps","outcome":"ok"}"#;
    let checked =
        |items: &str| format!(r#"{{"schema":1,"mode":"check","complete":true,"items":[{items}]}}"#);
    let ready = checked(&format!("{clt},{steps}"));
    assert!(naming_root_report(&ready));
    assert!(naming_root_report(&format!(
        "diagnostic\n{}\n",
        checked(&format!("{steps},{clt}"))
    )));
    for report in [
        checked(clt),
        checked(&format!("{clt},{clt}")),
        checked(&format!("{clt},{steps},{steps}")),
        ready.replace("\"mode\":\"check\"", "\"mode\":\"apply\""),
        ready.replace("\"complete\":true", "\"complete\":false"),
        ready.replace("\"schema\":1", "\"schema\":2"),
        ready.replace("\"outcome\":\"ok\"", "\"outcome\":\"deferred\""),
        ready.replace("\"outcome\":\"ok\"", "\"outcome\":\"not-applicable\""),
    ] {
        assert!(!naming_root_report(&report), "{report}");
    }

    let idle = format!(
        "OK schema={} setup=unread:spotlight busy=0 changing=0 queued=0 retrying=1 finished=0\n",
        aterm_types::macos_setup::SETUP_STATUS_SCHEMA
    );
    let progress = r#"{"profile":"macos-workstation-v1","format":1,"state":"pending","done":1,"running":null}"#;
    assert!(
        naming_setup_idle(&idle, progress),
        "unrelated unfinished setup does not block the separate root checkpoint"
    );
    for status in [
        idle.replace("busy=0", "busy=1"),
        idle.replace("changing=0", "changing=1"),
        idle.replace("queued=0", "queued=1"),
        idle.replace("busy=0", "busy=0 busy=1"),
        idle.replace("busy=0", ""),
        idle.replace(
            &format!("schema={}", aterm_types::macos_setup::SETUP_STATUS_SCHEMA),
            "schema=0",
        ),
    ] {
        assert!(!naming_setup_idle(&status, progress), "{status}");
    }
    for record in [
        progress.replace(",\"running\":null", ""),
        progress.replace("\"running\":null", "\"running\":\"unknown\""),
        progress.replace("\"running\":null", "\"running\":1"),
        progress.replace("\"state\":\"pending\"", "\"state\":\"unknown\""),
        progress.replace("\"format\":1", "\"format\":2"),
        progress.replace("\"done\":1", "\"done\":64"),
        String::new(),
    ] {
        assert!(!naming_setup_idle(&idle, &record), "{record}");
    }
    assert!(naming_episode_ended("done fixture 100\n"));
    for episode in [
        "",
        "opened\n",
        "unfinished fixture 100 1 99\n",
        "declined fixture 100 99 1\n",
        "asking fixture 42 10 100 1 0 0 99\n",
        "asking fixture 42 10 100 1 0 0 99 after done fixture 99\n",
        "done fixture 100 trailing\n",
    ] {
        assert!(!naming_episode_ended(episode), "{episode}");
    }

    let json = r#"{"schema":1,"items":[{"id":"a","elapsed_ms":4},{"id":"b"}],"phases":{"gather":10,"lock":2}}"#;
    let j = read_report_json(json).unwrap();
    assert_eq!((j.rows, j.timed), (2, 1));
    assert_eq!(j.phases["gather"], 10);
    assert_eq!(j.slowest, [("a".to_owned(), 4)]);
    // A report from before rows were timed: rows, no time, no phases.
    let old = read_report_json(r#"{"schema":1,"items":[{"id":"a"}]}"#).unwrap();
    assert_eq!((old.rows, old.timed, old.phases.len()), (1, 0, 0));
    assert!(old.slowest.is_empty());
    assert_eq!(read_report_json("not json"), None);
    // On a terminal the report follows the prompts' lines; the slowest rows come first.
    let tty = "Secrets vault URL or path (none skips): none\r\n\
               {\"items\":[{\"id\":\"a\",\"elapsed_ms\":4},{\"id\":\"b\",\"elapsed_ms\":90},\
               {\"id\":\"c\",\"elapsed_ms\":7},{\"id\":\"d\",\"elapsed_ms\":7}]}\r\n";
    let j = read_report_json(tty).unwrap();
    assert_eq!((j.rows, j.timed), (4, 4));
    assert_eq!(
        j.slowest,
        [
            ("b".to_owned(), 90),
            ("c".to_owned(), 7),
            ("d".to_owned(), 7)
        ]
    );
}

#[test]
fn an_init_exit_names_the_rows_it_left_open() {
    // On a terminal: the prompts' lines, a change, and the rows left open with their whys.
    let text = "Secrets vault URL or path (none skips): none\r\n\
                changed: Secrets vault (secrets.vault)\r\n    synced into ~/.secrets\r\n\
                failed: Command Line Tools (base.clt)\r\n    failed: not (this.why)\r\n\
                waiting: GitHub sign-in (github.auth@alice)\r\n\
                aterm init: 30 ok \u{b7} 1 changed \u{b7} 1 failed\r\n";
    assert_eq!(
        open_rows(text),
        ["failed base.clt", "waiting github.auth@alice"]
    );
    // A `--check` run's rows that still differ read `planned`, and fail it.
    let check = "planned: Caps Lock on keyboard 1452-1-0 (keyboard.caps-lock.1452-1-0@admin)\r\n\
                 needs action: Debugger access (init.permissions.debugger)\r\n";
    assert_eq!(
        open_rows(check),
        [
            "planned keyboard.caps-lock.1452-1-0@admin",
            "needs action init.permissions.debugger"
        ]
    );
    // A `--json` report names each row's outcome, after the prompts' lines.
    let json = "Secrets vault URL or path (none skips): none\r\n\
                {\"items\":[{\"id\":\"a\",\"outcome\":\"ok\"},{\"id\":\"base.clt\",\"outcome\":\"failed\"},\
                {\"id\":\"p\",\"outcome\":\"person\"},{\"id\":\"n\",\"outcome\":\"not-applicable\"},\
                {\"id\":\"c\",\"outcome\":\"changed\"}]}\r\n";
    assert_eq!(open_rows(json), ["failed base.clt", "person p"]);
    assert!(open_rows("aterm init: 41 ok\r\n").is_empty());
    // The plain text names a row by its title alone: every round's `--check` exit 1 read as
    // naming no row before this.
    let plain = "deferred: Workstation setup at this Mac's console\r\n    needs a person at \
                 this Mac's console (a coding agent started this): run aterm init there\r\n\
                 planned: the ALab toolset (every agent CLI included)\r\n    the toolset is \
                 incomplete\r\naterm init: 19 ok \u{b7} 1 to change \u{b7} 1 needs you\r\n";
    assert_eq!(
        open_rows(plain),
        [
            "deferred Workstation setup at this Mac's console",
            "planned the ALab toolset (every agent CLI included)"
        ]
    );

    let mut row = Row::new(Scenario::VaultLocal);
    let mut d = Drive {
        exit: Some(0),
        ..Drive::default()
    };
    open_rows_cell(&mut row, "", &d, text);
    assert!(row.cells.is_empty(), "a clean exit names nothing");
    d.exit = Some(1);
    open_rows_cell(&mut row, "", &d, text);
    open_rows_cell(&mut row, "again: ", &d, "aterm init: 1 failed\r\n");
    assert_eq!(
        row.cells
            .iter()
            .map(|(k, v)| format!("{k} = {v}"))
            .collect::<Vec<_>>(),
        [
            "rows left open = failed base.clt, waiting github.auth@alice",
            "again: rows left open = NOT MEASURED (init's output names no row left open)",
        ]
    );
}

#[test]
fn a_refused_step_is_not_run_and_a_failed_one_fails_the_run() {
    let refused = Row::refused(
        Scenario::Vault,
        &not_run("an ALab vault answers on the guest's network"),
    );
    assert_eq!(
        refused.outcome,
        Outcome::NotRun("an ALab vault answers on the guest's network".into())
    );
    let failed = Row::refused(Scenario::Doctor, &Fail::die("guest `x`: exit 1"));
    assert_eq!(failed.outcome, Outcome::Failed("guest `x`: exit 1".into()));
    let measured = Row::new(Scenario::InitApply);
    assert_eq!(exit_code(std::slice::from_ref(&measured)), 0);
    assert_eq!(exit_code(&[measured.clone(), refused.clone()]), NOT_RUN);
    assert_eq!(exit_code(&[refused.clone(), failed.clone()]), 1);
    assert_eq!(exit_code(&[]), NOT_RUN);
    let shown = table("head", &[refused, failed]);
    assert!(
        shown.starts_with("head\n\n| Row | Result | Measured |"),
        "{shown}"
    );
    assert!(
        shown.contains("| vault | NOT RUN: an ALab vault"),
        "{shown}"
    );
    assert!(
        shown.contains("| doctor | FAILED: guest `x`: exit 1 |"),
        "{shown}"
    );
}

#[test]
fn a_guard_failure_outlives_a_body_the_guest_could_not_run() {
    // E3 (2026-11-05): the guard fails the row, then the body's refusal is folded in.
    let mut row = Row::new(Scenario::Vault);
    row.cell("dialogs", Value::Text("none".into()));
    row.fail("the guest heard an ALab member answer".into());
    row.refuse(&not_run("an ALab vault answers on the guest's network"));
    assert_eq!(
        row.outcome,
        Outcome::Failed("the guest heard an ALab member answer".into())
    );
    assert_eq!(exit_code(std::slice::from_ref(&row)), 1);
    assert_eq!(row.cells[0].0, "dialogs", "the row keeps what it measured");
    assert_eq!(row.cells[1].0, "also not run");
    // NEGATIVE CONTROL: the row the old flow returned for the same run, the body's refusal
    // alone with every cell dropped, exits NOT RUN and hides the guard's failure.
    let old = Row::refused(
        Scenario::Vault,
        &not_run("an ALab vault answers on the guest's network"),
    );
    assert_eq!(exit_code(std::slice::from_ref(&old)), NOT_RUN);
    assert!(old.cells.is_empty());
    // A failed step after a NOT RUN one fails the row; a second failure is a cell.
    let mut row = Row::new(Scenario::Doctor);
    row.refuse(&not_run("no tart"));
    row.refuse(&Fail::die("guest `x`: exit 1"));
    row.refuse(&Fail::die("guest `y`: exit 2"));
    assert_eq!(row.outcome, Outcome::Failed("guest `x`: exit 1".into()));
    assert_eq!(
        row.cells,
        vec![(
            "also failed".into(),
            Value::Text("guest `y`: exit 2".into())
        )]
    );
}

/// A vault question is a prompt whose words name a vault, whatever its case; a sudo password
/// prompt is none. NEGATIVE CONTROL: the same drive with its vault prompt, which init asked
/// before `f83256665c`, names it, and the row fails on it.
#[test]
fn a_vault_question_is_a_prompt_naming_a_vault() {
    let asked = read_drive(
        "start_epoch_ms 1\nfirst_byte_ms 5\nprompt 120 50 Vault join password (Enter keeps none):\n\
         prompt 300 20 Password:\nend_ms 900\nprompts 2\n",
    );
    assert_eq!(vault_questions(&asked).len(), 1, "{asked:?}");
    let none = read_drive("start_epoch_ms 1\nprompt 300 20 Password:\nend_ms 900\nprompts 1\n");
    assert!(vault_questions(&none).is_empty(), "{none:?}");
}

/// The guest is in its own vault only when the vault's entry landed with the admin's bytes:
/// an entry that never came (0.155.0's init asked for no join password, so nothing was
/// typed) or that differs fails the row. NEGATIVE CONTROL: the row as it was, with no verdict,
/// measured such a run (E3's second batch, 2026-11-05).
#[test]
fn a_guests_own_vault_lets_it_in_only_with_the_vaults_entry_on_disk() {
    assert!(local_join_verdict(true, Some(true)).is_ok());
    let never = local_join_verdict(false, None).unwrap_err();
    assert!(
        never.contains("never wrote ~/.secrets/NUX_PROBE"),
        "{never}"
    );
    assert!(local_join_verdict(true, Some(false)).is_err());
    assert!(local_join_verdict(true, None).is_err());
}

#[test]
fn a_guests_join_with_no_url_must_refuse_as_a_lab_identity() {
    let refused = "aterm members join: this Mac runs in a virtual machine, so its vault \
                   identity is a lab identity (vm): a lab identity founds its own lab; pass \
                   its URL\nexit=1\n";
    let said = lab_join_verdict(refused).unwrap();
    assert!(said.starts_with("refused before it dialed: "), "{said}");
    // NEGATIVE CONTROL: a join that asked (no lab refusal) fails, whatever it printed.
    let asked = "Asked m22 to let this Mac in; waiting for a person's Allow\nexit=0\n";
    assert!(
        lab_join_verdict(asked)
            .unwrap_err()
            .contains("did not refuse")
    );
    assert!(lab_join_verdict("").is_err());
    assert_eq!(
        status_field("OK schema=1 fleet=listening setup=done", "fleet"),
        Some("listening")
    );
    assert_eq!(status_field("OK fleetx=1 fleet= ", "fleet"), Some(""));
    assert_eq!(status_field("OK setup=done", "fleet"), None);
}

#[test]
fn a_reach_probe_that_printed_nothing_never_reads_as_a_reached_vault() {
    assert_eq!(
        reach_of("192.168.1.242\n000\n"),
        ("192.168.1.242".into(), "000".into())
    );
    assert_eq!(reach_of("none\n200\n"), ("none".into(), "200".into()));
    // A resolver that printed nothing (the Command Line Tools' python stub): curl's code is
    // the first line, which is no address, and no second line is no answer.
    assert_eq!(reach_of("000\n"), ("none".into(), "000".into()));
    assert_eq!(reach_of(""), ("none".into(), "000".into()));
    assert_eq!(reach_of("none\nxyz\n"), ("none".into(), "000".into()));
}

#[test]
fn a_cell_never_breaks_the_table() {
    let mut row = Row::new(Scenario::InitApply);
    row.cell("last line", Value::Text("a | b\nc".into()));
    let shown = table("", &[row]);
    assert!(shown.contains("a \\| b c"), "{shown}");
    assert_eq!(shown.lines().count(), 5);
}

#[test]
fn a_line_counts_only_when_it_prints_outside_its_escapes() {
    let out = "\u{1b}]0;aterm\u{7}\r\n\u{1b}[1;32m\u{1b}[0m\r\n  \r\n\
               aterm init: 3 changed, 40 ok\r\n\u{1b}[2Kdone\n\u{1b}]7;file://h/x\u{1b}\\\n";
    assert_eq!(output_lines(out), 2);
    assert_eq!(output_lines(""), 0);
    let mut row = Row::new(Scenario::InitApply);
    lines_cell(&mut row, "no-op: ", out);
    assert_eq!(row.cells, [("no-op: lines".to_owned(), Value::Count(2))]);
}

#[test]
fn a_shell_session_ends_at_the_marker_its_prompt_prints() {
    use std::io::{BufRead as _, Write as _};

    const CHILD: &str = "ATERM_NUX_DRIVER_TEST";
    const NAME: &str = "mac_nux::tests::a_shell_session_ends_at_the_marker_its_prompt_prints";
    if let Ok(mode) = std::env::var(CHILD) {
        let mut input = std::io::stdin().lock();
        let mut output = std::io::stdout().lock();
        let mut reply = String::new();
        if mode == "shell" {
            writeln!(output, "\n{SHELL_READY}").unwrap();
            output.flush().unwrap();
            input.read_line(&mut reply).unwrap();
            assert_eq!(reply.trim_end(), "exit");
        } else {
            write!(output, "\nMac password: ").unwrap();
            output.flush().unwrap();
            input.read_line(&mut reply).unwrap();
            assert_eq!(reply.trim_end(), "driver-password");
            write!(output, "{{\"schema\":1,\"mode\":\"apply\"").unwrap();
            output.flush().unwrap();
            // Cross the old 1.5 s heuristic; ordinary unfinished output asks nothing.
            let mut fds = [crate::sys::pollfd(0)];
            let unexpected = crate::sys::poll(&mut fds, 3000).unwrap() != 0;
            writeln!(output).unwrap();
            output.flush().unwrap();
            std::process::exit(if unexpected { 42 } else { 0 });
        }
        std::process::exit(0);
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let script = scratch.path().join("drive.exp");
    std::fs::write(&script, DRIVE).unwrap();
    let exe = std::env::current_exe().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let drive = |mode: &str| {
        let record = scratch.path().join(format!("{mode}.rec"));
        let mut command = aterm_uds::spawnfd::command("/usr/bin/expect");
        command
            .arg("-f")
            .arg(&script)
            .arg(&record)
            .arg("60")
            .arg(&exe)
            .args([NAME, "--exact", "--nocapture"])
            .env(CHILD, mode)
            .env("NUX_SUDO", "driver-password")
            .env_remove("NUX_SHELL");
        if mode == "shell" {
            command.env("NUX_SHELL", "1");
        }
        let out = aterm_uds::deadline::output_until(&mut command, deadline).unwrap();
        assert!(out.status.success(), "{out:?}");
        let d = read_drive(&std::fs::read_to_string(record).unwrap());
        (d, String::from_utf8(out.stdout).unwrap())
    };
    let (quiet, _) = drive("json");
    let (shell, shell_out) = drive("shell");
    assert_eq!(quiet.exit, Some(0), "unsolicited input: {quiet:?}");
    assert_eq!(quiet.prompts.len(), 1, "{quiet:?}");
    assert_eq!(quiet.prompts[0].2, "Mac password:");
    assert_eq!(shell.exit, Some(0), "{shell:?}");
    assert!(shell.prompts.is_empty(), "{shell:?}");
    assert!(shell.shell_prompt_ms.is_some(), "{shell:?}");
    let mut actual = Row::new(Scenario::TerminalLaunch);
    shell_cells(&mut actual, "", &shell, &shell_out);
    assert_eq!(actual.outcome, Outcome::Measured);

    let record = "start_epoch_ms 10\nfirst_byte_ms 40\nshell_prompt_ms 812\nend_ms 900\n\
                  prompts 0\nquiet_waited_ms 0\nexit 0\n";
    let d = read_drive(record);
    assert_eq!(d.shell_prompt_ms, Some(812));
    let out =
        "aterm: setup continues in aterm.app (aterm help doctor)\r\nnux-shell-ready\r\n% exit\r\n";
    let mut row = Row::new(Scenario::TerminalLaunch);
    shell_cells(&mut row, "cold: ", &d, out);
    assert_eq!(row.outcome, Outcome::Measured);
    assert_eq!(
        row.cells[0],
        ("cold: to the shell prompt".to_owned(), Value::Ms(812))
    );
    assert_eq!(
        row.cells[1],
        ("cold: before it: lines".to_owned(), Value::Count(1))
    );
    // NEGATIVE CONTROL: no prompt is NOT MEASURED and fails the row, never 0.
    let mut row = Row::new(Scenario::TerminalLaunch);
    shell_cells(
        &mut row,
        "",
        &read_drive("start_epoch_ms 1\nhang 300000\n"),
        "",
    );
    assert!(matches!(row.cells[0].1, Value::Missing(_)));
    assert!(matches!(row.outcome, Outcome::Failed(_)));
}

#[test]
fn a_driver_that_left_no_record_fails_the_row_and_the_first_cause_stays() {
    let mut row = Row::new(Scenario::Doctor);
    drive_cells(&mut row, "terminal: ", &read_drive(""));
    assert_eq!(
        row.outcome,
        Outcome::Failed("terminal: the driver left no record".into())
    );
    drive_cells(
        &mut row,
        "read-only: ",
        &read_drive("start_epoch_ms 1\nhang 9\n"),
    );
    assert_eq!(
        row.outcome,
        Outcome::Failed("terminal: the driver left no record".into())
    );
    assert!(row.cells.iter().any(|(name, _)| name == "read-only: end"));
}

#[test]
fn percentiles_are_nearest_rank_and_exec_counts_read_lastcomm() {
    let ten: Vec<u64> = (1..=10).map(|n| n * 100).collect();
    assert_eq!(nearest_rank(&ten, 50), Some(500));
    assert_eq!(nearest_rank(&ten, 95), Some(1000));
    assert_eq!(nearest_rank(&[7], 95), Some(7));
    assert_eq!(nearest_rank(&[], 50), None);

    let lastcomm = "defaults         -       admin    __         0.00 secs Sat Oct 25 10:00 (0:00:00.02)\n\
                    killall          -       admin    __         0.00 secs Sat Oct 25 10:00 (0:00:00.01)\n\
                    defaults         -       admin    __         0.00 secs Sat Oct 25 10:00 (0:00:00.02)\n\
                    aterm            -       admin    __         0.10 secs Sat Oct 25 10:00 (0:00:01.00)\n";
    assert_eq!(exec_count(lastcomm, "defaults"), 2);
    assert_eq!(exec_count(lastcomm, "killall"), 1);
    assert_eq!(exec_count(lastcomm, "default"), 0);
    assert_eq!(exec_count("", "defaults"), 0);
}

#[test]
fn the_local_vault_is_served_on_loopback_and_never_advertised() {
    // A vault the guest advertised could be heard on the host's network: its server binds
    // only 127.0.0.1 and names no mDNS service.
    let setup = local_vault_setup("/Applications/aterm.app/Contents/MacOS/aterm", "/tmp/a");
    let serve = setup
        .split(';')
        .find(|step| step.contains(" secrets serve "))
        .unwrap();
    let words: Vec<&str> = serve.split_whitespace().collect();
    let listen = words
        .iter()
        .position(|w| *w == "--listen")
        .map(|at| words[at + 1])
        .unwrap();
    assert!(listen.starts_with("127.0.0.1:"), "{serve}");
    assert!(words.contains(&"--no-advertise"), "{serve}");
    assert!(!words.contains(&"--advertise"), "{serve}");
}

/// A run beside the pass says, in its row, a JSON report's completeness and its unresolved
/// rows, or its printed lines with the terminal's escape sequences dropped.
#[test]
fn a_run_beside_the_pass_says_its_unresolved_rows_or_its_lines() {
    let json = r#"{"schema":1,"mode":"apply","complete":false,"items":[{"id":"base.shell.rc","outcome":"blocked","why":"the hooks are not current"},{"id":"base.pkg.toolset","outcome":"planned","from":"no ALab programs are installed"}],"readiness":{"all_green":false,"unresolved":["base.shell.rc","base.pkg.toolset"]}}"#;
    assert_eq!(
        said_beside_the_pass(&format!("a prompt line\n{json}\n")),
        "complete false; unresolved: base.shell.rc blocked: the hooks are not current; \
         base.pkg.toolset planned: no ALab programs are installed"
    );
    let tty = "\u{1b}[1mrepair:\u{1b}[0m ok\r\n\u{1b}]0;aterm\u{7}\n\nwaits /\rwaits -\r\u{1b}[2K\
               2 rows | done\n";
    assert_eq!(said_beside_the_pass(tty), "repair: ok / 2 rows \u{a6} done");
}
