// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_census_reads_its_start_each_window_and_when_each_went() {
    let text = "census 1000 every=250 windows=2\n\
                base 1000 10 300 25 1512x33 Window Server\n\
                base 1000 11 301 0 800x600 Finder\n\
                new 4000 40 800 0 900x644 aterm\n\
                new 5250 41 120 0 420x210 nehelper\n\
                new 5500 42 121 0 260x300 SecurityAgent\n\
                gone 7000 41\n\
                new 9000 43 120 0 420x210 nehelper\n\
                new 9100 44 77 0 300x80 Install Command Line Developer Tools\n\
                torn line\n";
    let c = read_census(text);
    assert_eq!(c.started, Some((1000, 2)));
    assert_eq!(c.base.len(), 2);
    assert_eq!(c.new.len(), 5);
    assert_eq!(c.new[1].owner, "nehelper");
    assert_eq!(c.new[4].owner, "Install Command Line Developer Tools");
    assert_eq!(c.gone.get(&41), Some(&7000));
    let d = dialogs(&c, 4000, &[]);
    assert_eq!(
        d.iter()
            .map(|d| (d.owner.as_str(), d.kind, d.first_ms, d.windows))
            .collect::<Vec<_>>(),
        [
            ("nehelper", "Local Network", 1250, 2),
            ("SecurityAgent", "authorization", 1500, 1),
            (
                "Install Command Line Developer Tools",
                "Xcode install stub",
                5100,
                1
            ),
        ],
        "the app's own window and the baseline are never dialogs"
    );
    assert!(
        dialogs(&c, 9200, &[]).is_empty(),
        "nothing came after the mark"
    );
    let failed = read_census("census-error 5 the window list is unreadable\n");
    assert_eq!(
        failed.error.as_deref(),
        Some("the window list is unreadable")
    );
    assert_eq!(failed.started, None);
}

#[test]
fn every_kind_of_dialog_the_target_counts_is_named() {
    for (owner, kind) in [
        ("nehelper", "Local Network"),
        ("CoreServicesUIAgent", "Gatekeeper"),
        ("SecurityAgent", "authorization"),
        ("UserNotificationCenter", "consent"),
        ("BackgroundTaskManagementAgent", "Background Items"),
        ("NotificationCenter", "notification"),
        ("universalAccessAuthWarn", "Accessibility"),
        ("Something New", "other"),
    ] {
        assert_eq!(dialog_kind(owner, None), kind, "{owner}");
    }
    // macOS 27 puts its Local Network question in a UserNotificationCenter window: its words
    // name it; that owner's other windows stay consent alerts.
    let question = "AXWindow; AXStaticText \"Allow “aterm” to find devices on local networks?\"; \
                    AXButton \"Allow\"; AXButton \"Don’t Allow\"";
    assert_eq!(
        dialog_kind("UserNotificationCenter", Some(question)),
        "Local Network"
    );
    assert_eq!(
        dialog_kind(
            "UserNotificationCenter",
            Some("AXStaticText \"Background Items Added\"")
        ),
        "consent"
    );
}

#[test]
fn the_local_network_question_names_the_app_it_asks_about() {
    let curly = "AXStaticText \"Allow “aterm” to find devices on local networks?\"";
    assert_eq!(local_network_asker(curly).as_deref(), Some("aterm"));
    let escaped =
        "AXStaticText \"Allow \\\"tart-guest-agent\\\" to find devices on local networks?\"";
    assert_eq!(
        local_network_asker(escaped).as_deref(),
        Some("tart-guest-agent")
    );
    assert_eq!(
        local_network_asker("AXStaticText \"“aterm” would like to access files\""),
        None,
        "another question"
    );
}

/// A UserNotificationCenter window is the Local Network question only by a reading of its own
/// process taken while it was up; the same owner's other window stays a consent alert.
#[test]
fn a_notification_centre_window_is_named_by_the_readers_words_of_it() {
    let c = read_census(
        "census 1000 every=250 windows=0\n\
         new 2000 40 300 0 420x210 UserNotificationCenter\n\
         gone 2600 40\n\
         new 5000 41 300 0 420x210 UserNotificationCenter\n",
    );
    let read = read_reader(
        "reader 900 trusted=yes\n\
         words 2100 300 UserNotificationCenter: AXStaticText \"Allow “aterm” to find devices on local networks?\"\n\
         words 5100 300 UserNotificationCenter: AXStaticText \"Background Items Added\"\n",
    );
    let d = dialogs(&c, 1000, &read.words);
    assert_eq!(
        d.iter()
            .map(|d| (d.kind, d.first_ms, d.windows))
            .collect::<Vec<_>>(),
        [("Local Network", 1000, 1), ("consent", 4000, 1)]
    );
    // Negative control: with no words, both are consent alerts, one dialog of two windows.
    let blind = dialogs(&c, 1000, &[]);
    assert_eq!(
        blind
            .iter()
            .map(|d| (d.kind, d.windows))
            .collect::<Vec<_>>(),
        [("consent", 2)]
    );
}

#[test]
fn the_readers_record_names_each_dialogs_owner_and_words() {
    let r = read_reader(
        "reader 100 trusted=yes\n\
         words 200 77 UserNotificationCenter: AXWindow; AXStaticText \"Background Items Added\"\n\
         words 300 80 CoreServicesUIAgent: AXStaticText \"\\\"kimi\\\" is an app downloaded: x\"\n\
         words garbled\n",
    );
    assert_eq!(r.trusted, Some(true));
    assert_eq!(
        r.words,
        vec![
            Words {
                at_ms: 200,
                pid: 77,
                owner: "UserNotificationCenter".to_owned(),
                words: "AXWindow; AXStaticText \"Background Items Added\"".to_owned()
            },
            Words {
                at_ms: 300,
                pid: 80,
                owner: "CoreServicesUIAgent".to_owned(),
                words: "AXStaticText \"\\\"kimi\\\" is an app downloaded: x\"".to_owned()
            },
        ],
        "the words after the owner's first ': ', a garbled line dropped"
    );
    assert_eq!(read_reader("reader 1 trusted=no\n").trusted, Some(false));
    assert_eq!(
        read_reader("").trusted,
        None,
        "no record: unknown, never a no"
    );
}

#[test]
fn an_input_proves_presence_only_when_it_reset_the_idle_clock() {
    let text = "person 100 trusted=yes post=yes\n\
                input 200 nudge idle_before_ns=4800000000 idle_after_ns=21000000 window_ns=21500000\n\
                seen 300 77 nehelper\n\
                input 900 click idle_before_ns=700000000 idle_after_ns=19000000 window_ns=20900000\n\
                click 950 120 nehelper Allow\n\
                gone 1300 77\n";
    let p = read_person(text);
    assert_eq!((p.trusted, p.may_post), (Some(true), Some(true)));
    assert_eq!(
        hid_proof(&p),
        HidProof::Reset {
            inputs: 2,
            worst_after_ns: 21_000_000
        }
    );
    assert_eq!(
        click_latencies(&p),
        [("nehelper".to_owned(), "Allow".to_owned(), 650)]
    );
    // Posted events that never reached the HID system leave the clock running: never a pass.
    let stuck = read_person(
        "input 200 nudge idle_before_ns=4800000000 idle_after_ns=4820000000 window_ns=20400000\n\
         input 5200 nudge idle_before_ns=9800000000 idle_after_ns=9820000000 window_ns=20300000\n",
    );
    assert!(matches!(hid_proof(&stuck), HidProof::NotMeasured(why) if why.starts_with("2 of 2")));
    // A key straight after a click, with the clock barely restarted before it, and a read a
    // loaded guest took 300 ms to make, each restarted the clock inside its window.
    let close = read_person(
        "input 900 click idle_before_ns=700000000 idle_after_ns=20000000 window_ns=20800000\n\
         input 960 key idle_before_ns=19000000 idle_after_ns=24000000 window_ns=61000000\n\
         input 6000 nudge idle_before_ns=5000000000 idle_after_ns=310000000 window_ns=320000000\n",
    );
    assert_eq!(
        hid_proof(&close),
        HidProof::Reset {
            inputs: 3,
            worst_after_ns: 310_000_000
        }
    );
    // A key the HID system never took leaves the clock counting from the click before it,
    // past the key's window.
    let missed = read_person(
        "input 900 click idle_before_ns=700000000 idle_after_ns=20000000 window_ns=20800000\n\
         input 960 key idle_before_ns=19000000 idle_after_ns=62000000 window_ns=40000000\n",
    );
    assert!(matches!(hid_proof(&missed), HidProof::NotMeasured(why) if why.starts_with("1 of 2")));
    assert!(matches!(
        hid_proof(&read_person("input 1 nudge idle_before_ns=? idle_after_ns=? window_ns=20000000\n")),
        HidProof::NotMeasured(why) if why.contains("unreadable")
    ));
    assert!(matches!(
        hid_proof(&Person::default()),
        HidProof::NotMeasured(_)
    ));
}

#[test]
fn the_watch_names_each_stage_start_the_completion_and_a_kill() {
    let rec = |state: &str, done: u64, running: Option<u64>| {
        let running = running.map_or("null".to_owned(), |b| b.to_string());
        format!(
            r#"{{"profile":"macos-workstation-v1","state":"{state}","done":{done},"running":{running},"revision":1}}"#
        )
    };
    let text = format!(
        "at 1000 absent\nat 2000 {}\nat 2500 {}\nat 2600 {}\nat 4000 {}\nkilled 4001 812 running=4 sent=yes\nat 9000 {}\nat 12000 {}\n",
        rec("pending", 0, None),
        rec("pending", 0, Some(1)),
        rec("pending", 0, Some(1)),
        rec("pending", 1, Some(4)),
        rec("pending", 1, Some(4)),
        rec("completed", 7, None),
    );
    let (looks, kills) = read_watch(&text);
    assert_eq!(looks.first().map(|l| l.text.clone()), Some(None));
    assert_eq!(kills, [(4001, 812, 4)]);
    assert_eq!(
        stage_starts(&looks),
        [(2500, 1), (4000, 4)],
        "a stage still running across looks started once"
    );
    assert_eq!(completed_at(&looks), Some(12000));
    assert_eq!(
        last_record(&looks).map(|r| r.to_string()).as_deref(),
        Some("state=completed done=Workstation,Spotlight,Permissions running=none")
    );
    // A watch that ended pending names what it said last, absent looks after it aside.
    let (pending, _) = read_watch(&format!(
        "at 1 {}\nat 2 absent\n",
        rec("pending", 6, Some(8))
    ));
    assert_eq!(completed_at(&pending), None);
    assert_eq!(
        last_record(&pending).map(|r| r.to_string()).as_deref(),
        Some("state=pending done=Spotlight,Permissions running=Secrets")
    );
    assert_eq!(last_record(&read_watch("at 1 absent\n").0), None);
    assert_eq!(stage_name(4), "Permissions");
    let (resumed, _) = read_watch(&format!(
        "at 1 {}\nat 2 {}\nat 3 {}\n",
        rec("pending", 0, Some(4)),
        rec("pending", 0, None),
        rec("pending", 0, Some(4))
    ));
    assert_eq!(
        stage_starts(&resumed),
        [(1, 4), (3, 4)],
        "a resumed stage starts again"
    );
}

#[test]
fn the_process_watch_names_each_app_process_its_start_and_its_going() {
    let p = read_procs(
        "procs 1000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm\n\
         up 1050 793 start=1020\n\
         down 92400 793\n\
         up 92600 1201 start=92550\n\
         torn line\n\
         up 92700 x start=1\n",
    );
    assert_eq!((p.started, p.error.as_deref()), (Some(1000), None));
    assert_eq!(
        runs(&p),
        [
            Run {
                pid: 793,
                start_ms: 1020,
                down_ms: Some(92400)
            },
            Run {
                pid: 1201,
                start_ms: 92550,
                down_ms: None
            }
        ]
    );
    // A pid used again: its going closes only the process still up; a going of a pid never
    // seen up is no process.
    let reused =
        read_procs("procs 1 every=50 exe=x\nup 2 7 start=2\ndown 3 7\nup 4 7 start=4\ndown 5 9\n");
    assert_eq!(
        runs(&reused),
        [
            Run {
                pid: 7,
                start_ms: 2,
                down_ms: Some(3)
            },
            Run {
                pid: 7,
                start_ms: 4,
                down_ms: None
            }
        ]
    );
    let failed = read_procs(
        "procs 1 every=50 exe=x\nprocs-error 2 the kernel's process table did not read\n",
    );
    assert_eq!(
        failed.error.as_deref(),
        Some("the kernel's process table did not read")
    );
    assert!(runs(&failed).is_empty());
}

#[test]
fn the_sheet_presses_are_the_persons_quit_and_reopen_or_later_and_nothing_else() {
    let p = read_person(
        "seen 100 77 System Settings\n\
         click 200 120 System Settings switch:aterm\n\
         sheet 290 120 System Settings: AXSheet; AXButton \"Later\"; AXButton \"Quit & Reopen\"\n\
         click 300 120 System Settings Quit_&_Reopen\n\
         click 400 121 UserNotificationCenter Allow\n\
         click 500 120 System Settings Later\n",
    );
    assert_eq!(
        sheet_presses(&p),
        [(300, "Quit & Reopen".to_owned()), (500, "Later".to_owned())]
    );
    assert_eq!(
        p.dialogs,
        [(
            290,
            "sheet".to_owned(),
            "System Settings: AXSheet; AXButton \"Later\"; AXButton \"Quit & Reopen\"".to_owned()
        )],
        "the sheet's controls are kept for the run's record"
    );
    // Negative control: a switch, an Allow and a Quit that is not the sheet's are no press on it.
    let none = read_person(
        "click 200 120 System Settings switch:aterm\n\
         click 400 121 UserNotificationCenter Allow\n\
         click 450 121 CoreServicesUIAgent Quit\n",
    );
    assert!(sheet_presses(&none).is_empty());
}

#[test]
fn the_quit_confirms_are_the_persons_quit_on_the_apps_question_and_nothing_else() {
    let p = read_person(
        "click 300 120 System Settings Quit_&_Reopen\n\
         sheet 5900 793 aterm (dev): AXWindow/AXDialog; AXStaticText \"Quit aterm?\"\n\
         click 6000 793 aterm (dev) quit-confirm\n",
    );
    assert_eq!(quit_confirms(&p), [6000]);
    assert_eq!(
        p.clicks[1],
        (6000, "aterm (dev)".to_owned(), QUIT_CONFIRM.to_owned()),
        "an owner with spaces keeps them"
    );
    assert_eq!(
        reopen_presses(&p),
        ReopenPresses {
            sheet: vec![(300, "Quit & Reopen".to_owned())],
            quit: vec![6000],
        },
        "the question's press is no press on the sheet"
    );
    assert_eq!(
        p.dialogs[0].1, "sheet",
        "its controls are kept for the record"
    );
    // Negative control: the sheet's own presses, an Allow and a Quit button elsewhere.
    let none = read_person(
        "click 300 120 System Settings Quit_&_Reopen\n\
         click 400 120 System Settings Later\n\
         click 450 121 CoreServicesUIAgent Quit\n\
         click 500 121 UserNotificationCenter Allow\n",
    );
    assert!(quit_confirms(&none).is_empty());
}

#[test]
fn the_developer_tools_acts_are_the_persons_on_that_page_and_nothing_else() {
    let p = read_person(
        "click 200 120 System Settings switch:aterm\n\
         devtools 1000 120 pane list=lacks switch=none\n\
         devtools 1100 120 drag tile=793 from=900,300 to=500,400\n\
         click 7000 120 System Settings devtools-drag\n\
         typed 9000 120 account 5 Managed_via_Tart unwatched System Settings\n\
         devtools 30000 120 open-panel 120 System Settings: AXSheet; AXButton \"Open\"\n\
         click 31000 120 System Settings devtools-plus\n\
         click 52000 120 System Settings devtools-goto\n\
         click 58000 120 System Settings devtools-open\n\
         devtools 60000 120 pane list=holds switch=off\n\
         click 66000 120 System Settings devtools-switch\n\
         devtools 66500 120 pane list=holds switch=on\n\
         typed 70000 1173 account 5 - unwatched SecurityAgent\n",
    );
    let acts = devtools_acts(&p);
    assert_eq!(
        acts.panes,
        [
            (1000, "list=lacks switch=none".to_owned()),
            (60000, "list=holds switch=off".to_owned()),
            (66500, "list=holds switch=on".to_owned()),
        ]
    );
    assert_eq!(acts.adds, [(7000, Add::Drag), (31000, Add::Plus)]);
    assert_eq!(
        acts.steps,
        [
            (52000, DEVTOOLS_GOTO.to_owned()),
            (58000, DEVTOOLS_OPEN.to_owned())
        ]
    );
    assert_eq!(acts.switches, [66000]);
    assert_eq!(acts.passwords, [9000], "System Settings' password alone");
    assert_eq!(acts.notes.len(), 2, "{:?}", acts.notes);
    assert!(acts.notes[0].1.starts_with("drag tile=793"));
    // A page the list did not read keeps its state, not its description.
    let unread = devtools_acts(&read_person(
        "devtools 1000 120 pane list=unread switch=none: AXGroup; AXStaticText \"Developer Tools\"\n",
    ));
    assert_eq!(unread.panes, [(1000, "list=unread switch=none".to_owned())]);
    // Negative control: another page's switch, the sheet's presses and an Allow are none of it.
    let none = devtools_acts(&read_person(
        "click 200 120 System Settings switch:aterm\n\
         click 300 120 System Settings Quit_&_Reopen\n\
         click 400 121 UserNotificationCenter Allow\n\
         sheet 290 120 System Settings: AXSheet\n",
    ));
    assert_eq!(
        none,
        DevtoolsActs::default(),
        "nothing of the Developer Tools page"
    );
}

#[test]
fn the_status_read_names_the_first_session_listed() {
    // A first-launch guest's tabs, listed while System State fronts the window.
    let ls = "763 0 s-e19818197180bf468468 - alive ~ meta=0 window=0 active=0 wfocus=1\n\
              763 1 s-0a1b2c3d4e5f60718293 - alive ~ meta=0 window=0 active=- wfocus=1 *\n";
    assert_eq!(first_session(ls), Some("s-e19818197180bf468468"));
    assert_eq!(first_session("aterm-ctl: ERR no instance answered\n"), None);
    assert_eq!(first_session(""), None);
}

#[test]
fn a_typed_line_goes_to_the_first_shell_never_the_tab_listed_first() {
    // A first launch with the Mac Setup tab listed ahead of the shell's.
    let ls = "763 0 s-aaaaaaaaaaaaaaaaaaaa - alive ~ meta=0 path=live program=aterm agent=-\n\
              763 1 s-bbbbbbbbbbbbbbbbbbbb - alive ~ meta=0 path=live program=zsh agent=-\n";
    assert_eq!(shell_session(ls), Some("s-bbbbbbbbbbbbbbbbbbbb"));
    // The negative control: the first listed is the one `first_session` picks.
    assert_eq!(first_session(ls), Some("s-aaaaaaaaaaaaaaaaaaaa"));
    assert_eq!(
        session_programs(ls),
        vec![
            ("s-aaaaaaaaaaaaaaaaaaaa", "aterm"),
            ("s-bbbbbbbbbbbbbbbbbbbb", "zsh")
        ]
    );
    // No shell listed, and a line with no program= at all: no tab to type in.
    let none = "763 0 s-aaaaaaaaaaaaaaaaaaaa - alive ~ meta=0 program=aterm\n\
                763 1 s-cccccccccccccccccccc - alive ~ meta=0\n";
    assert_eq!(shell_session(none), None);
    assert_eq!(session_programs(none)[1], ("s-cccccccccccccccccccc", "-"));
}

#[test]
fn setup_reads_its_status_field_and_falls_back_to_the_record() {
    let line = "OK sid=1 fleet=off setup=pending:fda,devtools fabric=absent";
    assert_eq!(setup_field(line), Some("pending:fda,devtools"));
    assert_eq!(setup_field("OK sid=1 fleet=off"), None);
    let completed = Some(Record {
        state: "completed".into(),
        done: 7,
        running: None,
    });
    assert!(
        Reading {
            setup: None,
            network: None,
            record: completed.clone(),
            acts: 0,
        }
        .done(),
        "a build before setup= is read by its record"
    );
    assert!(
        !Reading {
            network: None,
            setup: Some("pending:fda".into()),
            record: completed.clone(),
            acts: 0,
        }
        .done(),
        "setup= wins once it is there"
    );
    assert!(
        !Reading {
            network: None,
            setup: Some("unread:dock;offered:fda".into()),
            record: completed.clone(),
            acts: 0,
        }
        .done(),
        "rows nothing read are not done, though the record completed"
    );
    // Review finding 12: a finished run read ten minutes on says `read-back:`, which is done
    // as last read; before, the lab waited on it as if a step were left.
    for (field, done) in [
        ("read-back:dock,capslock", true),
        ("read-back:dock;at-login:trackpad;offered:fda", true),
        ("done;asking:vault", true),
        // Off the ALab LAN the vault is asked again by itself and never answers: before, the
        // lab's watch waited on it until its stall detector.
        ("asking:vault", true),
        ("asking:vault;read-back:dock;offered:fda", true),
        ("asking:vault;changed:dock", true),
        ("done;changed:dock", true),
        ("asking:", false),
        ("unread:dock;read-back:capslock", false),
        ("pending:fda;read-back:dock", false),
        ("read-back:", false),
    ] {
        assert_eq!(
            Reading {
                network: Some("remote".into()),
                setup: Some(field.into()),
                record: completed.clone(),
                acts: 0,
            }
            .done(),
            done,
            "{field}"
        );
    }
    assert_eq!(
        read_record(r#"{"state":"pending","done":3,"running":8}"#),
        Some(Record {
            state: "pending".into(),
            done: 3,
            running: Some(8)
        })
    );
    assert_eq!(read_record("not json"), None);
}

#[test]
fn an_unanswered_request_finishes_only_for_the_vault_proven_off_its_lan() {
    for status in [
        "net=remote",
        "net=offline",
        "net=alab-lan",
        "net=checking",
        "net=-",
        "",
        "net=future-place",
        "internet=down net_kind=remote",
    ] {
        for setup in [
            "asking:vault",
            "asking:dock",
            "asking:vault,dock",
            "asking:",
        ] {
            let reading = Reading {
                setup: Some(setup.into()),
                network: network_field(status).map(str::to_owned),
                ..Default::default()
            };
            let expected =
                setup == "asking:vault" && matches!(status, "net=remote" | "net=offline");
            assert_eq!(reading.done(), expected, "{setup} {status}");
            assert_eq!(
                Stall::new(STALL_MS).see(0, &reading),
                if expected {
                    Waited::Done
                } else {
                    Waited::Moving
                },
                "{setup} {status}"
            );
        }
    }
    let status = "OK setup=asking:vault net=remote net_kind=hotspot";
    assert_eq!(setup_field(status), Some("asking:vault"));
    assert_eq!(network_field(status), Some("remote"));
}

#[test]
fn the_wait_stops_a_minute_after_setup_last_moved_and_never_before() {
    let at = |setup: &str| Reading {
        network: None,
        setup: Some(setup.to_owned()),
        record: None,
        acts: 0,
    };
    let mut wait = Stall::new(STALL_MS);
    assert_eq!(wait.see(0, &at("pending")), Waited::Moving);
    assert_eq!(wait.see(59_999, &at("pending")), Waited::Moving);
    // It moved: the minute starts again from the move.
    assert_eq!(wait.see(60_000, &at("pending:fda")), Waited::Moving);
    assert_eq!(wait.see(119_999, &at("pending:fda")), Waited::Moving);
    assert_eq!(
        wait.see(120_000, &at("pending:fda")),
        Waited::Stalled {
            stood: "setup=pending:fda (no record)".into(),
            since_ms: 60_000
        }
    );
    let mut done = Stall::new(STALL_MS);
    assert_eq!(done.see(5, &at("done")), Waited::Done);
    let mut offered = Stall::new(60_000);
    assert_eq!(offered.see(5, &at("done;offered:debugger")), Waited::Done);
    // A stage moving under an unchanged setup= is progress too.
    let mut stages = Stall::new(STALL_MS);
    let running = |bit| Reading {
        network: None,
        setup: Some("pending".into()),
        record: Some(Record {
            state: "pending".into(),
            done: 0,
            running: Some(bit),
        }),
        acts: 0,
    };
    assert_eq!(stages.see(0, &running(1)), Waited::Moving);
    assert_eq!(stages.see(50_000, &running(2)), Waited::Moving);
    assert_eq!(stages.see(100_000, &running(2)), Waited::Moving);
    assert!(matches!(
        stages.see(110_000, &running(2)),
        Waited::Stalled { .. }
    ));
}

/// A press aterm could have made is a row that waited on one (`;needs:`) while nobody asked
/// for anything; once a person asked for macOS's batch, the grants only a person gives are
/// theirs. Grants on offer are no press. Negative control: a run whose readings name none.
#[test]
fn presses_are_the_rows_that_waited_on_one_and_offers_are_not() {
    assert_eq!(
        setup_rows("pending:vault,repos;needs:repos", "needs"),
        ["repos"]
    );
    assert_eq!(
        setup_rows("done;asking:vault;offered:fda,debugger", "offered"),
        ["fda", "debugger"]
    );
    assert!(setup_rows("done;offered:fda", "needs").is_empty());
    let mut presses = Presses::default();
    for field in [
        "pending",
        "pending:vault;needs:debugger",
        "pending:vault,repos;needs:repos,debugger",
        "pending:lan;needs:lan,fda",
        "done;asking:vault;offered:fda",
    ] {
        presses.see(field);
    }
    assert_eq!(presses.needs, ["debugger", "repos", "lan", "fda"]);
    assert_eq!(presses.offered, ["fda"]);
    assert_eq!(
        presses.avoidable(false),
        ["debugger", "repos", "lan", "fda"]
    );
    // Someone at the console: a privacy switch is theirs, but the Local Network question is
    // no press at all (the LAN broker), so a `lan` card that waited fails the row.
    assert_eq!(presses.avoidable(true), ["repos", "lan"]);
    assert!(presses.local_network_step());
    let mut unread = Presses::default();
    unread.see("pending:repos;unread:dock");
    unread.see("unread:dock,spotlight;offered:fda");
    assert!(unread.needs.is_empty(), "an unread row waits on no press");
    assert_eq!(unread.named, ["repos", "dock", "spotlight", "fda"]);
    let mut none = Presses::default();
    none.see("pending:vault");
    none.see("done;asking:vault;offered:debugger");
    assert!(none.avoidable(false).is_empty(), "CONTROL: {none:?}");
    assert!(!none.local_network_step(), "CONTROL: {none:?}");
    let mut pending = Presses::default();
    pending.see("pending:vault,lan");
    assert!(pending.avoidable(false).is_empty());
    assert!(
        pending.local_network_step(),
        "a Local Network row named while pending is a step"
    );
}

/// Apple's developer-tools dialog is told from every other window the census lists.
#[test]
fn apples_developer_tools_dialog_is_picked_out_of_the_census() {
    let c = read_census(
        "census 1000 every=250 windows=0\n\
         new 2000 40 800 0 900x644 UserNotificationCenter\n\
         new 3000 44 77 0 300x80 Install Command Line Developer Tools\n",
    );
    let all = dialogs(&c, 1000, &[]);
    let apple = apple_devtools_dialogs(&all);
    assert_eq!(apple.len(), 1);
    assert_eq!(apple[0].first_ms, 2000);
    let quiet = read_census(
        "census 1000 every=250 windows=0\nnew 2000 40 800 0 900x644 UserNotificationCenter\n",
    );
    assert!(apple_devtools_dialogs(&dialogs(&quiet, 1000, &[])).is_empty());
    // The name macOS 27 gave the installer's window, as the control row's census read it.
    assert_eq!(
        dialog_kind("Install Command Line Developer", None),
        APPLE_DEVTOOLS_DIALOG
    );
    assert_eq!(
        dialog_kind("Install Command Line Tools", None),
        APPLE_DEVTOOLS_DIALOG
    );
    assert_eq!(dialog_kind("Installer", None), "other");
}

/// The person's and the processes' records as the guest helper writes them, from a run
/// shaped like 2026-10-28's 20:00 first launch: setup= stood still from 154.6 s while the
/// person turned Full Disk Access on, typed the password, answered the Quit & Reopen sheet
/// and aterm's question, and aterm quit and opened again.
const RECORDS: &str = "procs 1000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm\n\
                       up 1100 890 start=1050\n\
                       person 1000 trusted=yes post=yes\n\
                       input 2000 nudge idle_before_ns=5 idle_after_ns=1 window_ns=1\n\
                       input 193500 click idle_before_ns=5 idle_after_ns=1 window_ns=1\n\
                       click 193500 1331 System Settings switch:\n\
                       typed 226500 1331 account 5 Managed_via_Tart unwatched System Settings\n\
                       sheet 262000 1331 System Settings: AXSheet; AXButton \"Quit & Reopen\"\n\
                       click 263500 1331 System Settings Quit_&_Reopen\n\
                       click 274100 890 aterm quit-confirm\n\
                       down 280400 890\n\
                       up 280700 1995 start=280650\n";

#[test]
fn the_persons_presses_and_the_apps_processes_are_progress_but_a_repeated_press_is_not() {
    // Every line up to a time, as the wait reads the records then.
    let by = |ms: u64| -> String {
        RECORDS
            .lines()
            .filter(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|t| t.parse::<u64>().ok())
                    .is_some_and(|t| t <= ms)
            })
            .map(|l| format!("{l}\n"))
            .collect()
    };
    assert_eq!(
        acts(&by(2_000)),
        1,
        "the first process alone: nudges count not"
    );
    assert_eq!(acts(&by(193_500)), 2);
    assert_eq!(acts(RECORDS), 7);
    assert_eq!(acts(""), 0);
    assert_eq!(acts("click\ntorn\n"), 0);
    // A running installer is an act at each second it was seen (`setup_reading`), and the
    // same second read twice is one.
    assert_eq!(
        acts("up 100 installer 100\nup 100 installer 100\nup 101 installer 101\n"),
        2
    );
    let at = |ms: u64| Reading {
        network: None,
        setup: Some("pending:devtools,debugger,vault".into()),
        record: None,
        acts: acts(&by(ms)),
    };
    // The run's looks: setup= never moved from 154.6 s, and the wait went on through each
    // press and the relaunch, each less than a minute after the one before.
    let mut wait = Stall::new(STALL_MS);
    for ms in (154_600..=341_000).step_by(1_000) {
        assert_eq!(wait.see(ms, &at(ms)), Waited::Moving, "at {ms} ms");
    }
    assert_eq!(
        wait.see(341_600, &at(341_600)),
        Waited::Stalled {
            stood: "setup=pending:devtools,debugger,vault (no record)".into(),
            since_ms: 281_600
        },
        "a minute after the look that saw the reopened process, nothing new: stalled"
    );
    // Negative control: the same records without the person's presses or the processes'
    // goings and comings stall a minute after setup= last moved, as before.
    let mut still = Stall::new(STALL_MS);
    let bare = Reading {
        acts: 1,
        ..at(2_000)
    };
    assert_eq!(still.see(154_600, &bare), Waited::Moving);
    assert!(matches!(
        still.see(214_600, &bare),
        Waited::Stalled {
            since_ms: 154_600,
            ..
        }
    ));
    // A press repeated on the same control (an Allow that stays) is no progress: the
    // minute runs from its first press.
    let again = |n: u64| -> String {
        (0..=n)
            .map(|k| {
                format!(
                    "click {} 1249 UserNotificationCenter Allow\n",
                    1_000 + 3_000 * k
                )
            })
            .collect()
    };
    assert_eq!(acts(&again(0)), acts(&again(40)));
    // The same press after another act is another dialog's: it counts again.
    let between = format!(
        "{}typed 5000 1331 account 5 x unwatched System Settings\n{}",
        again(0),
        again(0)
    );
    assert_eq!(acts(&between), 3);
    let mut stuck = Stall::new(STALL_MS);
    let pressing = |n: u64| Reading {
        acts: acts(&again(n)),
        ..at(0)
    };
    assert_eq!(stuck.see(1_000, &pressing(0)), Waited::Moving);
    assert_eq!(stuck.see(60_999, &pressing(19)), Waited::Moving);
    assert!(matches!(
        stuck.see(61_000, &pressing(20)),
        Waited::Stalled {
            since_ms: 1_000,
            ..
        }
    ));
}
