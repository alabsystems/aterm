// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct ShapeFake {
    before: aterm_json::Value,
    after: aterm_json::Value,
    events: Vec<&'static str>,
    sets: Vec<(Shape, bool)>,
    boots: usize,
    fail_get: Option<&'static str>,
    fail_set: bool,
    interval: Option<DiskInterval>,
}

fn disk_report(disk: aterm_json::Value) -> aterm_json::Value {
    aterm_json::json!({"CPU":4,"Memory":8192,"Disk":disk,"Running":false,"State":"stopped"})
}

fn shape_fake(before: aterm_json::Value, after: aterm_json::Value) -> ShapeFake {
    ShapeFake {
        before,
        after,
        events: Vec::new(),
        sets: Vec::new(),
        boots: 0,
        fail_get: None,
        fail_set: false,
        interval: None,
    }
}

fn exercise_shape(vm: &mut ShapeFake) -> Result<(), Fail> {
    let shape = Shape {
        cpus: 4,
        memory_mib: 8192,
        disk_gb: 60,
    };
    configure_then_boot(
        vm,
        Some(shape),
        |vm, shape| {
            let interval = configure_shape(
                vm,
                shape,
                |vm, phase| {
                    if phase == "after" {
                        assert_eq!(vm.events.last(), Some(&"set"));
                    }
                    vm.events.push(phase);
                    if vm.fail_get == Some(phase) {
                        return Err(Fail::die("unavailable complete Get"));
                    }
                    Ok(if phase == "before" {
                        vm.before.clone()
                    } else {
                        vm.after.clone()
                    })
                },
                |vm, shape, grow| {
                    assert_eq!(vm.events.last(), Some(&"before"));
                    vm.events.push("set");
                    vm.sets.push((shape, grow));
                    if vm.fail_set {
                        Err(Fail::die("setter refused"))
                    } else {
                        Ok(())
                    }
                },
            )?;
            vm.interval = Some(interval);
            Ok(())
        },
        |vm| {
            assert_eq!(vm.events.last(), Some(&"after"));
            vm.events.push("boot");
            vm.boots += 1;
            Ok(())
        },
    )
}

#[test]
fn equal_disk_bucket_never_resizes_and_reapplies_cpu_memory_each_boot() {
    let mut vm = shape_fake(
        disk_report(aterm_json::json!(60)),
        disk_report(aterm_json::json!(60)),
    );
    for _ in 0..2 {
        exercise_shape(&mut vm).unwrap();
    }
    assert_eq!(vm.boots, 2);
    assert_eq!(
        vm.sets,
        vec![
            (
                Shape {
                    cpus: 4,
                    memory_mib: 8192,
                    disk_gb: 60
                },
                false
            );
            2
        ]
    );
    assert_eq!(
        vm.events,
        [
            "before", "set", "after", "boot", "before", "set", "after", "boot"
        ]
    );
    let interval = vm.interval.unwrap();
    // The bucket does not reveal the ASIF sector rounding; only its reported interval is known.
    assert_eq!(interval.reported_lower_bytes, 60_000_000_000);
    assert_eq!(interval.capacity_upper_exclusive_bytes, 61_000_000_000);
}

#[test]
fn only_a_smaller_disk_bucket_requests_growth_then_verifies_the_result() {
    let mut vm = shape_fake(
        disk_report(aterm_json::json!(59)),
        disk_report(aterm_json::json!(60)),
    );
    exercise_shape(&mut vm).unwrap();
    assert!(vm.sets[0].1);
    assert_eq!(vm.boots, 1);
    let mut unchanged = shape_fake(
        disk_report(aterm_json::json!(59)),
        disk_report(aterm_json::json!(59)),
    );
    assert!(exercise_shape(&mut unchanged).is_err());
    assert_eq!(unchanged.boots, 0);
}

#[test]
fn unknown_or_oversized_preconfiguration_never_sets_or_boots() {
    for disk in [
        aterm_json::json!(61),
        aterm_json::json!(null),
        aterm_json::json!("60"),
        aterm_json::json!(60.5),
        aterm_json::json!(-1),
    ] {
        let mut vm = shape_fake(disk_report(disk), disk_report(aterm_json::json!(60)));
        assert!(exercise_shape(&mut vm).is_err());
        assert!(vm.sets.is_empty());
        assert_eq!(vm.boots, 0);
    }
    let mut missing = shape_fake(
        aterm_json::json!({"Running":false,"State":"stopped"}),
        disk_report(aterm_json::json!(60)),
    );
    assert!(exercise_shape(&mut missing).is_err());
    assert!(missing.sets.is_empty());
}

#[test]
fn every_failed_step_or_unproved_postcondition_blocks_boot() {
    let report = disk_report(aterm_json::json!(60));
    for (key, value) in [
        ("CPU", aterm_json::json!(5)),
        ("Memory", aterm_json::json!(8193)),
        ("Disk", aterm_json::json!(61)),
        ("Disk", aterm_json::json!(null)),
        ("Running", aterm_json::json!(true)),
        ("State", aterm_json::json!("suspended")),
    ] {
        let mut after = report.clone();
        let _ = after.as_object_mut().unwrap().insert(key.to_owned(), value);
        let mut vm = shape_fake(report.clone(), after);
        assert!(exercise_shape(&mut vm).is_err());
        assert_eq!(vm.boots, 0);
    }
    for failure in 0..3 {
        let mut vm = shape_fake(report.clone(), report.clone());
        match failure {
            0 => vm.fail_get = Some("before"),
            1 => vm.fail_set = true,
            _ => vm.fail_get = Some("after"),
        };
        assert!(exercise_shape(&mut vm).is_err());
        assert_eq!(vm.boots, 0);
    }
}

#[test]
fn bounded_boot_configures_before_each_boot() {
    #[derive(Default)]
    struct Fake {
        configured: Option<Shape>,
        configurations: usize,
        boots: usize,
    }
    let shape = Shape {
        cpus: 4,
        memory_mib: 8192,
        disk_gb: 60,
    };
    let mut vm = Fake::default();
    for _ in 0..2 {
        configure_then_boot(
            &mut vm,
            Some(shape),
            |vm, observed| {
                assert_eq!(observed, shape);
                vm.configured = Some(observed);
                vm.configurations += 1;
                Ok(())
            },
            |vm| {
                // Taking the configured state makes a later boot require a fresh configuration.
                assert_eq!(vm.configured.take(), Some(shape));
                vm.boots += 1;
                Ok(())
            },
        )
        .unwrap();
    }
    assert_eq!((vm.configurations, vm.boots), (2, 2));
}

#[test]
fn configuration_failure_never_boots() {
    let mut booted = false;
    let shape = Shape {
        cpus: 4,
        memory_mib: 8192,
        disk_gb: 60,
    };
    let error = configure_then_boot(
        &mut booted,
        Some(shape),
        |_, _| Err(Fail::exit(7, "configuration refused")),
        |booted| {
            *booted = true;
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, 7);
    assert!(!booted);
}

#[test]
fn bounded_preparation_cache_cannot_reuse_another_shape_or_an_unbounded_base() {
    let shape = Shape {
        cpus: 4,
        memory_mib: 8192,
        disk_gb: 60,
    };
    let names = [
        None,
        Some(shape),
        Some(Shape { cpus: 5, ..shape }),
        Some(Shape {
            memory_mib: 8193,
            ..shape
        }),
        Some(Shape {
            disk_gb: 61,
            ..shape
        }),
    ]
    .map(|shape| {
        prep_name(
            IMAGE,
            &Prep {
                shape,
                ..Prep::default()
            },
        )
    });
    let unique: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len());
}

#[test]
fn the_grants_read_an_implicit_designated_requirement_too() {
    use std::io::Write as _;
    // codesign -d -r- on macOS 27: a linker-signed helper's line, then a Developer ID one's.
    let shown = "Executable=/usr/local/libexec/aterm-lab-guest\n\
                 # designated => cdhash H\"c5f7083cddd57a1d4bb8f57a1b55c34fc41b84a2\"\n\
                 designated => identifier \"x\" and anchor apple generic\n";
    let mut sed = aterm_uds::spawnfd::command("/usr/bin/sed")
        .args(["-n", DESIGNATED])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    sed.stdin
        .take()
        .unwrap()
        .write_all(shown.as_bytes())
        .unwrap();
    let out = sed.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "cdhash H\"c5f7083cddd57a1d4bb8f57a1b55c34fc41b84a2\"\n\
         identifier \"x\" and anchor apple generic\n"
    );
}

#[test]
fn every_build_of_the_helper_signed_with_its_identifier_meets_the_grants_requirement() {
    // Two builds differ in their bytes, so in their cdhash; each, signed ad hoc with PERSON_ID
    // as person_install signs it, meets the requirement the base's grants name. One signed with
    // another identifier does not. Neither copy is ever run.
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let meets = |build: &str, id: &str| {
        let copy = tmp.path().join(format!("{id}-{}", build.replace('/', "-")));
        std::fs::copy(build, &copy).unwrap();
        let signed = aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--identifier", id])
            .arg(&copy)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--verify", &format!("-R={}", person_requirement())])
            .arg(&copy)
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(meets("/usr/bin/true", PERSON_ID));
    assert!(meets("/usr/bin/false", PERSON_ID));
    assert!(!meets("/usr/bin/true", "aterm-lab-other"));
}

#[test]
fn tart_is_the_named_one_alone_else_homebrews_else_the_fetched_one() {
    let none = |_: &str| false;
    let all = |_: &str| true;
    let fetched = "/c/tart-x/tart.app/Contents/MacOS/tart";
    let at = |p: &str| TartSource::At(p.to_owned());
    assert_eq!(
        pick_tart(Some("/x/tart"), fetched, all).unwrap(),
        at("/x/tart")
    );
    assert_eq!(
        pick_tart(Some("rel/tart"), fetched, all).unwrap_err().code,
        2
    );
    // A named tart that does not run is NOT RUN, never another in its place.
    assert_eq!(
        pick_tart(Some("/x/tart"), fetched, none).unwrap_err().code,
        NOT_RUN
    );
    assert_eq!(pick_tart(None, fetched, all).unwrap(), at(TART_PATHS[0]));
    assert_eq!(
        pick_tart(None, fetched, |p: &str| p == TART_PATHS[1]).unwrap(),
        at(TART_PATHS[1])
    );
    // Homebrew's first; then the copy fetched before; then a fetch.
    assert_eq!(
        pick_tart(None, fetched, |p: &str| p == fetched).unwrap(),
        at(fetched)
    );
    assert_eq!(pick_tart(None, fetched, none).unwrap(), TartSource::Fetch);
}

#[test]
fn only_a_named_team_is_a_signing_team() {
    let report = "Executable=/t/tart.app/Contents/MacOS/tart\n\
                  Identifier=com.github.cirruslabs.tart\n\
                  TeamIdentifier=9M2P8L4D89\n";
    assert_eq!(signing_team(report), Some(TART_TEAM));
    // Ad hoc: codesign says so instead of a team.
    assert_eq!(
        signing_team("Signature=adhoc\nTeamIdentifier=not set\n"),
        None
    );
    assert_eq!(signing_team("Identifier=x\n"), None);
}

#[test]
fn a_person_base_is_out_of_reach_of_an_older_labs_sweep() {
    // A lab from before identifier grants deletes every `<kind>-person-…` base but its own when
    // it prepares one; this lab's base must not be among them, or an older checkout's lab
    // deletes it each time it prepares its own.
    for no_clt in [false, true] {
        let kind = prep_name(
            IMAGE,
            &Prep {
                no_clt,
                ..Prep::default()
            },
        );
        let person = prep_name(
            IMAGE,
            &Prep {
                no_clt,
                person: Some("aterm-lab-guest".to_owned()),
                ..Prep::default()
            },
        );
        assert!(person.starts_with(&kind), "{person}");
        assert!(!person.starts_with(&format!("{kind}-person-")), "{person}");
        // An allowed base is a kind of its own, and never under the old person prefix.
        let allowed = prep_name(
            IMAGE,
            &Prep {
                no_clt,
                lan_allow: true,
                ..Prep::default()
            },
        );
        assert_eq!(allowed, format!("{kind}-lanallow"));
        assert!(
            !allowed.starts_with(&format!("{kind}-person-")),
            "{allowed}"
        );
    }
}

#[test]
fn retained_runs_leave_older_person_bases_alone() {
    let mut prep = Prep {
        person: Some("aterm-lab-guest".to_owned()),
        keep: true,
        ..Prep::default()
    };
    let base = prep_name(IMAGE, &prep);
    retire_older_person_bases(
        IMAGE,
        &prep,
        &base,
        || panic!("a retained run has no reason to list older bases"),
        |name| panic!("a retained run tried to delete {name}"),
    )
    .unwrap();

    prep.keep = false;
    let older = format!("{}-person-digest", base_name(IMAGE, false));
    let names = format!("{base}\n{older}\naterm-mac-dmg-base-other-person-digest\n");
    let mut retired = Vec::new();
    retire_older_person_bases(
        IMAGE,
        &prep,
        &base,
        || Ok(names),
        |name| {
            retired.push(name.to_owned());
        },
    )
    .unwrap();
    assert_eq!(retired, [older]);
}

#[test]
fn retained_runs_refuse_sip_off_and_preserve_the_base() {
    let base = base_name(IMAGE, false);
    let disabled = "System Integrity Protection status: disabled.";
    let refused = require_sip_on(disabled, &base, true, |name| {
        panic!("a retained run tried to delete {name}")
    })
    .unwrap_err();
    assert_eq!(refused.code, NOT_RUN);

    let mut retired = Vec::new();
    let refused = require_sip_on(disabled, &base, false, |name| {
        retired.push(name.to_owned());
        true
    })
    .unwrap_err();
    assert_eq!(refused.code, NOT_RUN);
    assert_eq!(retired, [base.clone()]);

    let enabled = "System Integrity Protection status: enabled.";
    require_sip_on(enabled, &base, false, |name| {
        panic!("a successful boot tried to delete {name}")
    })
    .unwrap();
}

#[test]
fn a_base_is_named_by_its_image_and_grant_and_is_never_a_lanes_clone() {
    let plain = base_name(IMAGE, false);
    let fda = base_name(IMAGE, true);
    assert_eq!(
        plain,
        "aterm-mac-dmg-base-ghcr-io-cirruslabs-macos-golden-gate-base-latest"
    );
    assert_eq!(fda, format!("{plain}-fda"));
    assert_ne!(
        base_name("ghcr.io/x:1", false),
        base_name("ghcr.io/x:2", false)
    );
    // A base is kept by the lane on purpose, never taken for an ended lane's clone.
    let names = format!("{plain}\n{fda}\naterm-mac-dmg-kept-7\naterm-mac-dmg-8\nother\n");
    assert_eq!(
        kept_by_lane(&names),
        [plain.as_str(), fda.as_str(), "aterm-mac-dmg-kept-7"]
    );
    assert_eq!(
        ended_lane_candidates(&names, 1, |_| false),
        ["aterm-mac-dmg-8"]
    );
}

#[test]
fn only_tccds_dialog_for_a_desktop_read_ends_the_controls_wait() {
    // tccd's lines for a `~/Desktop` read with no grant, 2026-10-21 (macOS 27.0 guest).
    let prompting = [
        r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyDesktopFolder, from Sub:{\/opt\/homebrew\/Cellar\/tart-guest-agent\/0.15.0\/bin\/tart-guest-agent}Resp:{…}"}"#,
        r#"{"eventMessage":"AUTHREQ_PROMPTING: msgID=158.27, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{\/opt\/homebrew\/Cellar\/tart-guest-agent\/0.15.0\/bin\/tart-guest-agent}Resp:{…}"}"#,
    ]
    .join("\n");
    assert!(desktop_prompted(&prompting));
    // A request alone, or a dialog for another service, is no dialog for the read.
    let asked = prompting.lines().next().unwrap();
    assert!(!desktop_prompted(asked));
    assert!(!desktop_prompted(
        r#"{"eventMessage":"AUTHREQ_PROMPTING: msgID=9.1, service=kTCCServiceSystemPolicyDocumentsFolder"}"#
    ));
    assert!(!desktop_prompted(
        r#"{"eventMessage":"AUTHREQ_CTX: msgID=158.27, function=TCCAccessRequest, service=kTCCServiceSystemPolicyDesktopFolder"}"#
    ));
    assert!(!desktop_prompted(""));
}

#[test]
fn a_tccd_event_is_read_by_its_message_never_its_format_string() {
    // A line as a macOS 27.0 guest's `log stream --style ndjson` writes it (2026-10-25), cut.
    let line = r#"{"timezoneName":"","formatString":"AUTHREQ_PROMPTING: msgID=%{public}@, service=%{public}@, subject=%{public}@, ","eventMessage":"AUTHREQ_PROMPTING: msgID=158.53, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{\/opt\/homebrew\/bin\/tart-guest-agent}Resp:{…}","processID":95}"#;
    assert_eq!(
        event_message(line),
        "AUTHREQ_PROMPTING: msgID=158.53, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{/opt/homebrew/bin/tart-guest-agent}Resp:{…}"
    );
    // The format string alone names no service, so it is no dialog for the read.
    assert!(!desktop_prompted(
        r#"{"formatString":"AUTHREQ_PROMPTING: msgID=%{public}@, service=kTCCServiceSystemPolicyDesktopFolder","eventMessage":"AUTHREQ_CTX: msgID=1.1"}"#
    ));
    assert!(desktop_prompted(line));
    assert_eq!(event_message("not json"), "not json");
}

#[test]
fn only_the_dmg_acceptance_executables_are_taken_by_package() {
    let lines = [
        r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/atpkg#0.107.0","target":{"name":"dmg_acceptance"},"executable":"/b/deps/dmg_acceptance-1"}"#,
        r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/atpkg#0.107.0","target":{"name":"atpkg"},"executable":"/b/deps/atpkg-9"}"#,
        r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/aterm-uds#0.107.0","target":{"name":"dmg_acceptance"},"executable":null}"#,
        r#"{"reason":"build-finished","success":true}"#,
        "not json",
        r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/aterm-update#0.107.0","target":{"name":"dmg_acceptance"},"executable":"/b/deps/dmg_acceptance-4"}"#,
    ]
    .join("\n");
    let exes = bench_executables(&lines);
    assert_eq!(
        exes.into_iter().collect::<Vec<_>>(),
        [
            (
                "aterm-update".to_owned(),
                PathBuf::from("/b/deps/dmg_acceptance-4")
            ),
            (
                "atpkg".to_owned(),
                PathBuf::from("/b/deps/dmg_acceptance-1")
            ),
        ]
    );
}

#[test]
fn a_package_is_named_by_its_id_whatever_its_folder() {
    let line = |id: &str| {
        format!(
            r#"{{"reason":"compiler-artifact","package_id":"{id}","target":{{"name":"dmg_acceptance"}},"executable":"/b/x"}}"#
        )
    };
    let exes = bench_executables(&line("path+file:///w/crates/uds#aterm-uds@0.107.0"));
    assert_eq!(exes.keys().collect::<Vec<_>>(), ["aterm-uds"]);
}

#[test]
fn a_bench_passes_only_on_its_own_complete_report_and_says_why_a_row_did_not() {
    let pass = "measure row: x::a PASS\nall ran\nmeasure row: x::b PASS\nok\n\
                measure result: x planned=2 passed=2 failed=0 refused=0\n";
    let (shown, verdict) = read_report(pass);
    assert_eq!(verdict, Verdict::Pass);
    assert_eq!(
        shown,
        [
            "measure row: x::a PASS",
            "measure row: x::b PASS",
            "measure result: x planned=2 passed=2 failed=0 refused=0"
        ]
    );

    let fail = "measure row: x::a FAIL\nmeasurement panicked: left 1 right 2\nat x.rs\n\
                measure row: x::b PASS\nok\nmeasure result: x planned=2 passed=1 failed=1 refused=0\n";
    let (shown, verdict) = read_report(fail);
    assert_eq!(verdict, Verdict::Fail);
    assert_eq!(
        shown[..3],
        [
            "measure row: x::a FAIL",
            "measurement panicked: left 1 right 2",
            "at x.rs"
        ]
    );

    let typed = "measure row: x::a COULD NOT RUN\nno hdiutil\n\
                 measure result: x planned=1 passed=0 failed=0 refused=1\n";
    assert_eq!(read_report(typed).1, Verdict::NotRun);
    assert_eq!(read_report(typed).0[1], "no hdiutil");
    let panicked = "measure row: x::a FAIL\n\
                    measurement panicked: aterm-gate: COULD NOT RUN — trustd did not answer\n\
                    measure result: x planned=1 passed=0 failed=1 refused=0\n";
    assert_eq!(read_report(panicked).1, Verdict::NotRun);
    let both = format!(
        "{}{fail}",
        typed.replace(
            "measure result: x planned=1 passed=0 failed=0 refused=1\n",
            ""
        )
    );
    assert_eq!(read_report(&both).1, Verdict::Fail);
    let later = "measure row: x::a FAIL\nleft != right\n\
                 measurement panicked: aterm-gate: COULD NOT RUN — quoted\n\
                 measure result: x planned=1 passed=0 failed=1 refused=0\n";
    assert_eq!(read_report(later).1, Verdict::Fail);

    assert_eq!(read_report("measure row: x::a PASS\n").1, Verdict::Fail);
    assert_eq!(read_report("tart exec: timed out").1, Verdict::Fail);
    let empty = "measure result: x planned=0 passed=0 failed=0 refused=0\n";
    assert_eq!(read_report(empty).1, Verdict::NotRun);
}

#[test]
fn ended_lane_candidates_exclude_live_and_kept_names() {
    let first = VmIdentity::allocate().unwrap();
    let second = VmIdentity::allocate().unwrap();
    assert_ne!(first.running_name(), second.running_name());
    assert_ne!(first.kept_name(), second.kept_name());
    let own = format!("{}\n{}\n", first.running_name(), second.running_name());
    assert_eq!(
        ended_lane_candidates(&own, 0, |_| false),
        [first.running_name(), second.running_name()],
        "both actual allocations carry recognizable owners"
    );
    assert!(
        ended_lane_candidates(&own, i32::try_from(std::process::id()).unwrap(), |_| false)
            .is_empty(),
        "the actual allocator's own clones are never reported as stale"
    );
    let kept = format!("{}\n{}\n", first.kept_name(), second.kept_name());
    assert_eq!(kept_by_lane(&kept).len(), 2);
    assert!(ended_lane_candidates(&kept, 300, |_| false).is_empty());
    let names = "aterm-mac-dmg-100\naterm-mac-dmg-100-1\naterm-mac-dmg-100-2\n\
                 aterm-mac-dmg-200\naterm-mac-dmg-200-1\naterm-mac-dmg-300\naterm-mac-dmg-300-1\n\
                 aterm-mac-dmg-kept-100\naterm-mac-dmg-kept-100-1\naterm-mac-dmg-base-image\n\
                 aterm-mac-dmg-\naterm-mac-dmg-1x\naterm-mac-dmg-100-\naterm-mac-dmg-100-0\n\
                 aterm-mac-dmg-0-1\naterm-mac-dmg-+100-1\naterm-mac-dmg-100-+1\n\
                 aterm-mac-dmg-100-1-extra\naterm-mac-dmg-2147483648-1\n\
                 aterm-mac-dmg-100-18446744073709551616\nother-vm\n";
    let alive = |pid: i32| pid == 200;
    assert_eq!(
        ended_lane_candidates(names, 300, alive),
        [
            "aterm-mac-dmg-100",
            "aterm-mac-dmg-100-1",
            "aterm-mac-dmg-100-2"
        ]
    );
    assert_eq!(ended_lane_candidates("", 300, alive), Vec::<String>::new());
}

#[test]
fn sip_is_on_only_when_wholly_enabled() {
    assert!(sip_on("System Integrity Protection status: enabled.\n"));
    assert!(!sip_on("System Integrity Protection status: disabled.\n"));
    assert!(!sip_on(
        "System Integrity Protection status: enabled (Custom Configuration).\n\n\
         Configuration:\n\tFilesystem Protections: disabled\n"
    ));
}

#[test]
fn tccd_lines_count_removable_requests_by_subject_and_desktop_requests() {
    let log = [
        r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyRemovableVolumes, from Sub:{com.aterm.aterm}Resp:{TCCDProcess: …}"}"#,
        r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyDesktopFolder, from Sub:{\/opt\/homebrew\/bin\/tart-guest-agent}Resp:{…}"}"#,
        r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyAllFiles, from Sub:{com.aterm.aterm}Resp:{…}"}"#,
        r#"{"eventMessage":"AUTHREQ_CTX: service=kTCCServiceSystemPolicyRemovableVolumes"}"#,
    ]
    .join("\n");
    assert_eq!(
        tcc_seen(&log),
        TccSeen {
            removable: vec!["com.aterm.aterm".to_owned()],
            desktop: 1,
        }
    );
    assert_eq!(tcc_seen(""), TccSeen::default());
}

#[test]
fn a_count_means_something_only_where_the_control_shows_consent_enforced() {
    let asked = TccSeen {
        removable: Vec::new(),
        desktop: 1,
    };
    let silent = TccSeen::default();
    assert!(control_verdict(false, false, &asked).is_ok());
    assert!(control_verdict(true, true, &silent).is_ok());
    assert!(control_verdict(false, true, &silent).is_err());
    assert!(control_verdict(false, false, &silent).is_err());
    assert!(control_verdict(false, true, &asked).is_err());
    assert!(control_verdict(true, false, &asked).is_err());
}
