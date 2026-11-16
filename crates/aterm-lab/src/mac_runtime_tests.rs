// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn sip_probe_retains_complete_facts_before_setup_and_preserves_the_gate() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;

    let enabled = b"System Integrity Protection status: enabled.\n".as_slice();
    for (name, status, stdout, expected) in [
        ("enabled", 0, enabled, None),
        (
            "disabled",
            0,
            b"System Integrity Protection status: disabled.\n".as_slice(),
            Some(crate::mac::NOT_RUN),
        ),
        ("empty", 0, b"".as_slice(), Some(crate::mac::NOT_RUN)),
        (
            "custom",
            0,
            b"System Integrity Protection status: unknown (Custom Configuration).\n".as_slice(),
            Some(crate::mac::NOT_RUN),
        ),
        ("nonzero", 7 << 8, enabled, Some(1)),
        ("signal", libc::SIGKILL, enabled, Some(1)),
    ] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let probe = SipProbe {
            tart: "/owned/tart",
            requested_image: "registry/image@sha256:requested",
            selected_base: "existing-prepared-base",
            guest: "owned-runtime-guest",
        };
        // Stderr is evidence, not the SIP acceptance authority. Keep its binary bytes too.
        let stderr = b"disabled in a diagnostic, not stdout\0\xff\n".to_vec();
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: stdout.to_vec(),
            stderr: stderr.clone(),
        };
        let status = output.status;
        let captures = Cell::new(0);
        let setup = Cell::new(false);
        let result = (|| {
            require_sip_on(tmp.path(), &probe, || {
                captures.set(captures.get() + 1);
                Ok(output)
            })?;
            let receipt: Value =
                aterm_json::from_slice(&std::fs::read(tmp.path().join("sip.json")).unwrap())
                    .unwrap();
            assert_eq!(receipt["output_complete"].as_bool(), Some(true));
            assert_eq!(
                std::fs::read(tmp.path().join("sip.stdout")).unwrap(),
                stdout
            );
            assert_eq!(
                std::fs::read(tmp.path().join("sip.stderr")).unwrap(),
                stderr
            );
            setup.set(true);
            Ok::<(), Fail>(())
        })();
        assert_eq!(captures.get(), 1, "{name}");
        assert_eq!(setup.get(), expected.is_none(), "{name}");
        match expected {
            Some(code) => assert_eq!(result.unwrap_err().code, code, "{name}"),
            None => result.unwrap(),
        }
        let receipt: Value =
            aterm_json::from_slice(&std::fs::read(tmp.path().join("sip.json")).unwrap()).unwrap();
        assert_eq!(receipt["output_complete"].as_bool(), Some(true), "{name}");
        assert_eq!(
            receipt["success"].as_bool(),
            Some(status.success()),
            "{name}"
        );
        assert_eq!(
            receipt["exit"].as_i64(),
            status.code().map(i64::from),
            "{name}"
        );
        assert_eq!(
            receipt["signal"].as_i64(),
            status.signal().map(i64::from),
            "{name}"
        );
        assert_eq!(
            receipt["stdout_bytes"].as_u64(),
            Some(stdout.len() as u64),
            "{name}"
        );
        assert_eq!(
            receipt["stderr_bytes"].as_u64(),
            Some(stderr.len() as u64),
            "{name}"
        );
        assert_eq!(
            receipt["probe"]["tart_path_reference"].as_str(),
            Some(probe.tart)
        );
        assert_eq!(
            receipt["probe"]["requested_image_reference"].as_str(),
            Some(probe.requested_image)
        );
        assert_eq!(
            receipt["probe"]["selected_base_reference"].as_str(),
            Some(probe.selected_base)
        );
        assert_eq!(
            receipt["probe"]["vm_name_reference"].as_str(),
            Some(probe.guest)
        );
        assert_eq!(
            receipt["probe"]["argv"],
            json!([
                probe.tart,
                "exec",
                probe.guest,
                "/bin/sh",
                "-c",
                SIP_STATUS_COMMAND
            ])
        );
        assert_eq!(
            std::fs::read(tmp.path().join("sip.stdout")).unwrap(),
            stdout,
            "{name}"
        );
        assert_eq!(
            std::fs::read(tmp.path().join("sip.stderr")).unwrap(),
            stderr,
            "{name}"
        );
    }
}

#[test]
fn sip_capture_failure_has_no_invented_status_or_complete_streams_and_never_sets_up() {
    use std::cell::Cell;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let probe = SipProbe {
        tart: "/owned/tart",
        requested_image: "requested-image",
        selected_base: "selected-base",
        guest: "owned-guest",
    };
    let setup = Cell::new(false);
    let result = (|| {
        require_sip_on(tmp.path(), &probe, || {
            Err("complete guest output: stdout exceeds its complete-output bound".into())
        })?;
        setup.set(true);
        Ok::<(), Fail>(())
    })();
    assert_eq!(result.unwrap_err().code, 1);
    assert!(!setup.get());
    let receipt: Value =
        aterm_json::from_slice(&std::fs::read(tmp.path().join("sip.json")).unwrap()).unwrap();
    assert_eq!(receipt["output_complete"].as_bool(), Some(false));
    for field in [
        "success",
        "exit",
        "signal",
        "stdout_file",
        "stderr_file",
        "stdout_bytes",
        "stderr_bytes",
    ] {
        assert!(receipt[field].is_null(), "{field}");
    }
    assert!(receipt["error"].as_str().is_some());
    assert!(!tmp.path().join("sip.stdout").exists());
    assert!(!tmp.path().join("sip.stderr").exists());
}

#[test]
fn sip_evidence_write_failure_cannot_admit_setup_or_publish_a_complete_record() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let blocked = tmp.path().join("sip.stderr");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("retained"), b"existing evidence").unwrap();
    let probe = SipProbe {
        tart: "/owned/tart",
        requested_image: "requested-image",
        selected_base: "selected-base",
        guest: "owned-guest",
    };
    let stdout = b"System Integrity Protection status: enabled.\n";
    let setup = Cell::new(false);
    let result = (|| {
        require_sip_on(tmp.path(), &probe, || {
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.to_vec(),
                stderr: b"new stderr".to_vec(),
            })
        })?;
        setup.set(true);
        Ok::<(), Fail>(())
    })();
    assert_eq!(result.unwrap_err().code, 1);
    assert!(!setup.get());
    assert_eq!(
        std::fs::read(tmp.path().join("sip.stdout"))
            .unwrap()
            .as_slice(),
        stdout
    );
    assert_eq!(
        std::fs::read(blocked.join("retained")).unwrap(),
        b"existing evidence"
    );
    assert!(!tmp.path().join("sip.json").exists());
}

#[test]
fn collection_observes_retained_raw_streams_and_cannot_erase_a_red_execution() {
    use std::os::unix::process::ExitStatusExt as _;

    let tmp = aterm_tempfile::tempdir().unwrap();
    for (phase, code, prior_codes, expected_refusal) in [
        ("red", 1, vec![Some(0)], 1),
        ("later-green", 0, vec![Some(1), Some(0)], 1),
        ("green", 0, vec![Some(0)], crate::mac::NOT_RUN),
    ] {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"original\0stdout\xff\n".to_vec(),
            stderr: b"original\0stderr\xfe\nreal 2\nuser 1\nsys 0.5\n".to_vec(),
        };
        let receipt = CaptureReceipt {
            schema: "aterm-runtime-capture-v1".into(),
            complete: true,
            exit: Some(code),
            signal: None,
            stdout_bytes: output.stdout.len(),
            stderr_bytes: output.stderr.len(),
            error: None,
        };
        let execution = tmp.path().join(format!("{phase}.execution.json"));
        let expected_output = output.clone();
        let refused = retain_phase(
            tmp.path(),
            phase,
            "the measured command",
            output,
            Ok(receipt),
            |_| {
                assert_eq!(
                    std::fs::read(tmp.path().join(format!("{phase}.stdout"))).unwrap(),
                    expected_output.stdout,
                    "collection must begin after the complete original stdout was retained"
                );
                assert_eq!(
                    std::fs::read(tmp.path().join(format!("{phase}.stderr"))).unwrap(),
                    expected_output.stderr,
                    "binary stderr is independent of text in the row log"
                );
                let row: Value =
                    aterm_json::from_slice(&std::fs::read(&execution).unwrap()).unwrap();
                assert_eq!(row["exit"].as_i64(), Some(i64::from(code)));
                assert_eq!(row["success"].as_bool(), Some(code == 0));
                assert_eq!(row["output_complete"].as_bool(), Some(true));
                assert_eq!(row["usage"]["real"].as_f64(), Some(2.));
                Err::<(), _>(not_run("injected artifact transport refusal"))
            },
        )
        .map_err(|error| preserve_failure(&prior_codes, error))
        .unwrap_err();
        assert_eq!(refused.code, expected_refusal);
        assert!(refused.msg.unwrap().contains("artifact transport refusal"));
        assert!(
            execution.is_file(),
            "the refusal must keep the execution record"
        );
        assert!(
            !tmp.path().join(format!("{phase}.json")).exists(),
            "a refused collection cannot publish a completed measurement row"
        );
    }
}

#[test]
fn invalid_or_incomplete_capture_retains_raw_bytes_without_claiming_completion() {
    use std::os::unix::process::ExitStatusExt as _;

    let tmp = aterm_tempfile::tempdir().unwrap();
    for (phase, defect, producer_exit, transport_exit, expected) in [
        ("malformed", "json", 0, 0, crate::mac::NOT_RUN),
        ("capped", "cap", 0, 0, crate::mac::NOT_RUN),
        ("late-capped-red", "cap-red", 1, 0, 1),
        ("wrong-length", "length", 1, 0, 1),
        ("failed-transfer", "transport", 1, 7, 1),
    ] {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(transport_exit << 8),
            stdout: b"raw\0stdout\xff".to_vec(),
            stderr: b"raw\0stderr\xfe".to_vec(),
        };
        let mut wire = json!({"schema":"aterm-runtime-capture-v1", "complete":true,
            "exit":producer_exit, "signal":null, "stdout_bytes":output.stdout.len(),
            "stderr_bytes":output.stderr.len(), "error":null});
        let fields = wire.as_object_mut().unwrap();
        match defect {
            "cap" | "cap-red" => {
                fields.insert("complete".into(), json!(false));
                if defect == "cap" {
                    fields.insert("exit".into(), Value::Null);
                }
                fields.insert(
                    "error".into(),
                    json!("stdout exceeds its complete-output bound"),
                );
            }
            "length" => {
                fields.insert("stdout_bytes".into(), json!(output.stdout.len() + 1));
            }
            _ => {}
        }
        let bytes = if defect == "json" {
            b"{".to_vec()
        } else {
            aterm_json::to_string(&wire).unwrap().into_bytes()
        };
        let expected_output = output.clone();
        let error = retain_phase(
            tmp.path(),
            phase,
            "producer",
            output,
            CaptureReceipt::parse(&bytes),
            |_| -> Result<(), Fail> {
                panic!("optional collection cannot start without complete producer output")
            },
        )
        .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(
            std::fs::read(tmp.path().join(format!("{phase}.stdout"))).unwrap(),
            expected_output.stdout
        );
        assert_eq!(
            std::fs::read(tmp.path().join(format!("{phase}.stderr"))).unwrap(),
            expected_output.stderr
        );
        let execution: Value = aterm_json::from_slice(
            &std::fs::read(tmp.path().join(format!("{phase}.execution.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(execution["output_complete"].as_bool(), Some(false));
    }
}

#[test]
fn sealed_direct_signal_survives_collection_failure() {
    use std::os::unix::process::ExitStatusExt as _;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let output = std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: b"before signal\n".to_vec(),
        stderr: Vec::new(),
    };
    let receipt = CaptureReceipt {
        schema: "aterm-runtime-capture-v1".into(),
        complete: true,
        exit: None,
        signal: Some(libc::SIGKILL),
        stdout_bytes: output.stdout.len(),
        stderr_bytes: 0,
        error: None,
    };
    let error = retain_phase(
        tmp.path(),
        "signalled",
        "producer",
        output,
        Ok(receipt),
        |output| {
            assert_eq!(output.status.signal(), Some(libc::SIGKILL));
            Err::<(), _>(not_run("optional collector unavailable"))
        },
    )
    .unwrap_err();
    assert_eq!(error.code, 1);
    let execution: Value = aterm_json::from_slice(
        &std::fs::read(tmp.path().join("signalled.execution.json")).unwrap(),
    )
    .unwrap();
    assert!(execution["exit"].is_null());
    assert_eq!(execution["signal"].as_i64(), Some(i64::from(libc::SIGKILL)));
    assert_eq!(execution["success"].as_bool(), Some(false));
    assert_eq!(execution["output_complete"].as_bool(), Some(true));
}

#[test]
fn an_existing_phase_directory_cannot_launch_or_supply_stale_completion() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("a driver's files");
    let directory = root.join("take-1");
    std::fs::create_dir_all(&directory).unwrap();
    let stale = aterm_json::to_string(&json!({"schema":"aterm-runtime-capture-v1",
        "complete":true, "exit":0, "signal":null, "stdout_bytes":0,
        "stderr_bytes":0, "error":null}))
    .unwrap()
    .into_bytes();
    assert!(
        CaptureReceipt::parse(&stale)
            .unwrap()
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(directory.join("capture.json"), &stale).unwrap();
    let marker = tmp.path().join("producer-ran");
    let run = |reservation: String| {
        aterm_uds::deadline::output_until(
            command(aterm_uds::deadline::stand_in(Path::new("/bin/sh"))).args([
                "-c",
                &format!(
                    "set -eu; {reservation}; printf invoked > {}",
                    q(&marker.display().to_string())
                ),
            ]),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap()
    };
    assert!(!run(reserve_phase(&root, "take-1")).status.success());
    assert!(!marker.exists());
    assert_eq!(
        std::fs::read(directory.join("capture.json")).unwrap(),
        stale
    );
    // Negative control: accepting this existing namespace permits the launch that
    // would then consume the already-present success receipt.
    assert!(
        run(format!("mkdir -p {}", q(&directory.display().to_string())))
            .status
            .success()
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"invoked");
}

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
    let record = PathBuf::from(record);
    let promoted = crate::sys::set_darwin_role(0, 1).is_ok();
    let role = crate::sys::darwin_role(0).unwrap();
    let temporary = record.with_extension("tmp");
    std::fs::write(&temporary, format!("{promoted} {role}\n")).unwrap();
    std::fs::rename(temporary, record).unwrap();
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
        let mut job = app_job(tmp.path(), &phase, &["/bin/sh".into(), "-c".into(), script]);
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
        while !record.is_file() {
            assert!(
                Instant::now() < until,
                "the native role probe did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(std::fs::read_to_string(&record).unwrap(), expected);
    }
}

#[test]
fn timing_job_preserves_argument_boundaries_and_separate_driver_diagnostics() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("a driver's files");
    let dir = root.join("take-2");
    std::fs::create_dir_all(&dir).unwrap();
    let command = "printf '%s' '<&>\\\"'; printf failure >&2; exit 17";
    let job = app_job(
        &root,
        "take-2",
        &[
            aterm_uds::deadline::stand_in(Path::new("/bin/sh"))
                .display()
                .to_string(),
            "-c".into(),
            command.into(),
        ],
    );
    assert_eq!(job["RunAtLoad"].as_bool(), Some(true));
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
    assert_eq!(result.status.code(), Some(17));
    assert_eq!(result.stdout, b"<&>\\\"");
    assert_eq!(result.stderr, b"failure");
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
