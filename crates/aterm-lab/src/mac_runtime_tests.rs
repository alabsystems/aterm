// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn paired_takes_counterbalance_order_and_drive_distinct_supplied_images() {
    let order = take_order(true);
    assert_eq!(
        &order[..4],
        &[
            (1, "baseline"),
            (1, "candidate"),
            (2, "candidate"),
            (2, "baseline")
        ]
    );
    for arm in ["baseline", "candidate"] {
        assert_eq!(
            order.iter().filter(|(_, name)| *name == arm).count(),
            TAKES as usize
        );
    }
    let command = lane_args("cat").join(" ");
    let selected = arm_command(&command, "baseline");
    assert_ne!(selected, arm_command(&command, "candidate"));
    assert!(selected.contains(&format!("{ROOT}/baseline.app/Contents/MacOS/aterm")));
    assert!(!selected.contains(&format!("{ROOT}/aterm.app")));
    assert_eq!(take_order(false).len(), TAKES as usize);
}

#[test]
fn offline_rank_paths_belong_to_the_selected_checkout_not_the_runner_directory() {
    let args = |words: &[&str]| words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        count_args(
            &args(&["--record", "docs/rows", "--candidate", "/scratch/default"]),
            Path::new("/selected")
        )
        .unwrap(),
        (
            PathBuf::from("/selected/docs/rows"),
            Some(PathBuf::from("/scratch/default"))
        )
    );
    assert_ne!(
        count_args(&args(&["--record", "docs/rows"]), Path::new("/other"))
            .unwrap()
            .0,
        PathBuf::from("/selected/docs/rows")
    );
    assert!(
        count_args(
            &args(&["--record", "r", "--other", "c"]),
            Path::new("/selected")
        )
        .is_err()
    );
    assert!(count_args(&args(&["--record"]), Path::new("/selected")).is_err());
}

#[test]
fn focused_rewind_uses_cargos_artifacts_and_keeps_controls_explicit() {
    let row = |name: &str, path: &str| {
        json!({"reason":"compiler-artifact", "target":{"name":name}, "executable":path}).to_string()
    };
    let artifacts = format!(
        "{}\n{}",
        row("rewind", &format!("{ROOT}/target.noindex/build/rewind")),
        row("fake-tui", &format!("{ROOT}/target.noindex/debug/fake-tui"))
    );
    let recording = rewind_command(&artifacts, false).unwrap();
    assert!(recording.contains("--filter recording_keeps_parse_throughput"));
    assert!(!recording.contains("--throughput-control"));
    let control = rewind_command(&artifacts, true).unwrap();
    assert!(control.contains("--throughput-control"));
    assert!(!control.contains("--filter"));
    assert!(rewind_command(&row("rewind", "/tmp/foreign"), false).is_err());
    assert!(
        rewind_command(
            &format!(
                "{artifacts}\n{}",
                row("rewind", &format!("{ROOT}/target.noindex/other"))
            ),
            false
        )
        .is_err(),
        "negative control: ambiguous cached builds cannot be guessed"
    );
    for lane in ["rewind", "rewind-control"] {
        assert!(
            parse(&[
                "--app".into(),
                "a".into(),
                "--record".into(),
                "r".into(),
                "--lane".into(),
                lane.into()
            ])
            .is_ok()
        );
    }
}

#[test]
fn runtime_role_probe() {
    let Some(record) = std::env::var_os("ATERM_Q4_ROLE_RECORD") else {
        return;
    };
    let promoted = crate::sys::set_darwin_role(0, 1).is_ok();
    let role = crate::sys::darwin_role(0).unwrap();
    std::fs::write(record, format!("{promoted} {role}\n")).unwrap();
}

/// Exercise the generated job through launchd. A plist string alone cannot establish the
/// task type its shell's children inherit; Interactive is the daemon negative control.
#[test]
fn timing_children_can_take_the_app_role_but_interactive_daemons_cannot() {
    use std::time::Instant;

    struct Loaded(String);
    impl Drop for Loaded {
        fn drop(&mut self) {
            let _ = aterm_uds::deadline::output_until(
                command("/bin/launchctl").args(["bootout", &self.0]),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            );
        }
    }

    let tmp = aterm_tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    for (mutation, expected) in [(None, "true 1\n"), (Some("Interactive"), "false 0\n")] {
        let phase = format!("role-{}-{}", std::process::id(), mutation.unwrap_or("App"));
        let dir = tmp.path().join(&phase);
        std::fs::create_dir(&dir).unwrap();
        let record = dir.join("role");
        let script = format!(
            "export ATERM_Q4_ROLE_RECORD={}; {} --exact mac_runtime::tests::runtime_role_probe --nocapture",
            q(&record.display().to_string()),
            q(&exe.display().to_string())
        );
        let mut job = app_job(tmp.path(), &phase, &script);
        if let Some(kind) = mutation {
            job.as_object_mut()
                .unwrap()
                .insert("ProcessType".into(), json!(kind));
        }
        let input = dir.join("job.json");
        let plist = dir.join("job.plist");
        write_json(&input, &job).unwrap();
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let converted = aterm_uds::deadline::output_until(
            command("/usr/bin/plutil").args([
                "-convert",
                "xml1",
                "-o",
                plist.to_str().unwrap(),
                input.to_str().unwrap(),
            ]),
            until,
        )
        .unwrap();
        assert!(converted.status.success(), "{converted:?}");
        let domain = format!("gui/{}", crate::sys::getuid());
        let target = format!("{domain}/{}", job["Label"].as_str().unwrap());
        let loaded = aterm_uds::deadline::output_until(
            command("/bin/launchctl").args(["bootstrap", &domain, plist.to_str().unwrap()]),
            until,
        )
        .unwrap();
        assert!(loaded.status.success(), "{loaded:?}");
        let _loaded = Loaded(target);
        while !dir.join("exit").is_file() {
            assert!(
                Instant::now() < until,
                "the native role probe did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(std::fs::read_to_string(dir.join("exit")).unwrap(), "0\n");
        assert_eq!(std::fs::read_to_string(&record).unwrap(), expected);
    }
}

#[test]
fn timing_job_preserves_arguments_and_separate_results() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("a driver's files");
    let dir = root.join("take-2");
    std::fs::create_dir_all(&dir).unwrap();
    let command = "printf '%s' '<&>\\\"'; printf failure >&2; exit 17";
    let job = app_job(&root, "take-2", command);
    assert_eq!(job["RunAtLoad"].as_bool(), Some(true));
    assert_eq!(job["ProgramArguments"][0].as_str(), Some("/bin/sh"));
    assert_eq!(job["ProgramArguments"][1].as_str(), Some("-c"));
    assert_ne!(job["StandardOutPath"], job["StandardErrorPath"]);
    let bytes = aterm_json::to_string(&job).unwrap();
    assert_eq!(aterm_json::from_str::<Value>(&bytes).unwrap(), job);
    let args = job["ProgramArguments"].as_array().unwrap();
    let result = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(args[0].as_str().unwrap())
            .args(args[1..].iter().map(|a| a.as_str().unwrap())),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"<&>\\\"");
    assert_eq!(result.stderr, b"failure");
    // A wrapper that loses the child's nonzero status fails this negative control.
    assert_eq!(std::fs::read_to_string(dir.join("exit")).unwrap(), "17\n");
    assert!(!dir.join("exit.tmp").exists());
}

#[test]
fn a_bad_source_lock_refuses_before_an_unlocked_compiler_is_probed() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("source");
    let home = tmp.path().join("home");
    let prefix = home.join("pkg");
    let bin = prefix.join("store/trust/current/bin");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["targo", "trustc"] {
        let file = bin.join(name);
        std::fs::write(&file, b"never executed: the probe guard refuses").unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"trust\"\n",
    )
    .unwrap();
    std::fs::write(root.join("trust.lock"), "not a lock").unwrap();
    let mut probes = 0;
    let mut before = |_: &Path| {
        probes += 1;
        Err("unexpected probe".into())
    };
    assert!(
        resolve_tools(
            &root,
            &home,
            &prefix,
            std::ffi::OsStr::new(""),
            None,
            &mut before
        )
        .is_err()
    );
    assert_eq!(probes, 0);
    // NEGATIVE CONTROL: without the lock this candidate really does reach a probe.
    std::fs::remove_file(root.join("trust.lock")).unwrap();
    let mut before = |_: &Path| {
        probes += 1;
        Err("probe stopped before exec".into())
    };
    assert!(
        resolve_tools(
            &root,
            &home,
            &prefix,
            std::ffi::OsStr::new(""),
            None,
            &mut before
        )
        .is_err()
    );
    assert!(probes > 0);
}

#[test]
fn every_take_measures_the_supplied_bundle_and_no_lane_can_publish() {
    let parse =
        |words: &[&str]| super::parse(&words.iter().map(|w| (*w).into()).collect::<Vec<_>>());
    assert!(parse(&["--app", "a", "--record", "r"]).is_ok());
    assert!(parse(&["--app", "a"]).is_err());
    assert!(parse(&["--app", "a", "--record", "r"]).unwrap().graphics);
    assert!(
        !parse(&["--app", "a", "--record", "r", "--display", "headless"])
            .unwrap()
            .graphics
    );
    assert!(parse(&["--app", "a", "--record", "r", "--lane", "release"]).is_err());
    assert!(parse(&["--app", "a", "--record", "r", "--lane", "measure-settle"]).is_err());
    for retired in ["sequential", "together", "interleaved"] {
        assert!(parse(&["--app", "a", "--record", "r", "--count-mode", retired]).is_err());
    }
    assert!(lane_args("measure").contains(&format!("{ROOT}/aterm.app")));
    // NEGATIVE CONTROL: cat's old unconditional build silently measured a different binary.
    let cat = lane_args("cat");
    let flag = cat.iter().position(|w| w == "--binary").unwrap();
    assert_eq!(
        cat[flag + 1],
        format!("{ROOT}/aterm.app/Contents/MacOS/aterm")
    );
    assert_eq!(
        lane_args("fabric"),
        ["perf", "fabric", "--profile", "shipped"]
    );
    assert!(environment(3).contains("CARGO_BUILD_JOBS=3;"));
    assert!(!environment(3).contains("CARGO_BUILD_JOBS=8;"));
}

#[test]
fn a_spread_needs_all_five_finite_takes_and_keeps_the_noise() {
    assert!(spread(&[1., 2., 3., 4.]).is_null());
    assert!(spread(&[1., 2., 3., 4., f64::NAN]).is_null());
    let s = spread(&[5., 2., 4., 1., 3.]);
    assert_eq!(s["median"].as_f64(), Some(3.));
    assert_eq!(s["range"].as_f64(), Some(4.));
    assert_eq!(s["takes"].as_array().unwrap().len(), 5);
    let r = usage("real 1.5\nuser 0.8\nsys 0.2\n123 maximum resident set size\n");
    assert_eq!(r["real"].as_f64(), Some(1.5));
    assert_eq!(r["peak_rss_bytes"].as_u64(), Some(123));
    assert!(usage("not a timing reading")["real"].is_null());
    assert_eq!(
        max_load(&[
            "{ 3.5 2.0 1.0 }".into(),
            "{ 8.0 4.0 3.0 }".into(),
            "{ 2.0 9.0 9.0 }".into()
        ]),
        Some(8.)
    );
    assert_eq!(max_load(&["unavailable".into(), "NaN".into()]), None);
    assert_eq!(experiment_exit(&[Some(0); 6], false), 0);
    assert_eq!(experiment_exit(&[Some(0); 12], true), 0);
    assert_eq!(experiment_exit(&[Some(0); 6], true), 3);
    assert_eq!(experiment_exit(&[Some(0); 5], false), 3);
    // A red warm-up stays red even if all five pre-planned takes pass.
    assert_eq!(
        experiment_exit(
            &[Some(1), Some(0), Some(0), Some(0), Some(0), Some(0)],
            false
        ),
        1
    );
    assert_eq!(
        experiment_exit(
            &[Some(3), Some(0), Some(0), Some(0), Some(0), Some(0)],
            false
        ),
        3
    );
}

#[test]
fn retained_rows_stay_under_the_guest_run_and_disk_units_match_tart() {
    let log =
        format!("log: {ROOT}/target.noindex/verify/.ubd/logs.noindex/runs.noindex/123-456.log");
    assert!(verifier_log(&log).is_some());
    assert!(verifier_log("log: /etc/shadow").is_none());
    assert!(verifier_log(&log.replace("123-456.log", "../secret.log")).is_none());
    assert!(verifier_log(&log.replace("runs.noindex/", "runs.noindex/../")).is_none());
    assert!(verifier_log(&log.replace(".ubd/", "../../")).is_none());
    assert!(verifier_log(&log.replace("123-456.log", "123-pid.log")).is_none());
    assert!(verifier_log(&log.replace("runs.noindex/", "runs.noindex/next/")).is_some());
    let ledgers = perf_ledgers();
    assert_eq!(
        ledger_member(&format!("{ledgers}/fabric/take-1.json")),
        Some("fabric/take-1.json")
    );
    assert_eq!(ledger_member(&format!("{ledgers}/../../secret")), None);
    assert_eq!(ledger_member(&format!("{ledgers}/fabric/../../x")), None);
    assert_eq!(ledger_member(&format!("{ledgers}.evil/take-1.json")), None);
    assert_eq!(ledger_member(&format!("{ledgers}/")), None);
    assert_eq!(ledger_member("fabric/take-1.json"), None);
    let df = |free| format!("Filesystem 1G-blocks Used Available\ndisk 100 0 {free}\n");
    assert!(disk_room(&df(84), 60)); // 84 GiB exceeds the 90 decimal GB disk + margin.
    assert!(!disk_room(&df(83), 60));
    assert!(!disk_room(&df(84), 80));
    assert!(!disk_room("unavailable", 60));
    let args = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let shape = parse(&args(
        "--app a --record r --cpus 4 --memory-gib 48 --disk-gb 80",
    ))
    .unwrap();
    assert_eq!(
        (shape.cpus, shape.memory_mib, shape.disk_gb),
        (4, 49152, 80)
    );
    assert!(parse(&args("--app a --record r --cpus 0")).is_err());
    assert!(parse(&args("--app a --record r --memory-gib 4294967295")).is_err());
}
