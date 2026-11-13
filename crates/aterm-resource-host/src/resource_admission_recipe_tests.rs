// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
#[cfg(target_os = "macos")]
fn capability_probe_child_keeps_original_sdk_gc_backing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: scratch.path().canonicalize().unwrap().join("manager"),
    };
    let build = layout.build_dir("trust", 1);
    std::fs::create_dir_all(&build).unwrap();
    let atpkg::lease::Hold::Held(lease) = atpkg::lease::hold_for(
        &layout.prefix,
        &build,
        "capability probe SDK backing",
        Duration::from_secs(30),
    )
    .unwrap() else {
        panic!("private original SDK lease unavailable");
    };
    let selection = compiler::Selection::NativeFixture {
        execution: build,
        build: 1,
        lease: std::sync::Arc::new(lease),
    };
    let mut probe = spawnfd::command("/usr/bin/true");
    probe.env_clear();
    assert!(configure_capability_backing(&mut probe, None, None).is_err());
    assert!(probe.guard_targets().next().is_none());
    configure_capability_backing(&mut probe, None, Some(&selection)).unwrap();
    assert_eq!(probe.guard_targets().count(), 1);
    let held = aterm_uds::deadline::Held::start(&mut probe).unwrap();
    drop(probe);
    drop(selection);
    let subject = atpkg::lease::Subject::build("trust", 1).unwrap();
    assert!(matches!(
        atpkg::lease::reclaim(&layout.prefix, &subject),
        atpkg::lease::Reclaim::Keep(atpkg::lease::Holders::Held(_))
    ));
    let output = held
        .output_until(
            crate::fixture_deadline(Instant::now()),
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(output.status.success());
    assert!(matches!(
        atpkg::lease::reclaim(&layout.prefix, &subject),
        atpkg::lease::Reclaim::Clear(_)
    ));
}

#[test]
#[cfg(target_os = "macos")]
fn caller_loader_overrides_refuse_before_an_actual_managed_ay_probe() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::recipe::tests::caller_loader_overrides_refuse_before_an_actual_managed_ay_probe",
    ) {
        return;
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let layout = atpkg::store::Layout {
        prefix: root.join("manager"),
    };
    let build = layout.build_dir("ay", 1);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::create_dir_all(layout.bin_dir()).unwrap();
    let program = build.join("bin/ay");
    std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    let runtime_data = build.join("runtime-data");
    std::fs::write(&runtime_data, b"signed original runtime data").unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    let name = atpkg::store::ToolName::new("ay").unwrap();
    atpkg::platform::install_shim_to(&layout.shim(&name), &program).unwrap();
    let registry = signed_tests::Registry::new(&build);
    let select = |layout: &atpkg::store::Layout| registry.acquire(layout);
    let marker = root.join("candidate-ran");
    let mut request = Submission {
        binding: Binding {
            session: "s-a".into(),
            attempt: [31; 16],
        },
        peer_pid: std::process::id(),
        peer_birth: 1,
        argv: vec![
            program.as_os_str().as_bytes().to_vec(),
            b"bench".to_vec(),
            b"run".to_vec(),
        ],
        cwd: root.as_os_str().as_bytes().to_vec(),
        env: vec![(
            b"ATERM_TEST_RECIPE_PROBE_MARKER".to_vec(),
            marker.as_os_str().as_bytes().to_vec(),
        )],
        cpu_units: 1,
        memory_bytes: 1024 * 1024 * 1024,
        label: "loader prebirth refusal".into(),
        timeout_ms: Some(60_000),
    };
    // This controlled owner uses its own actual signing anchor. It does
    // not replace production's pinned anchor or grant resource custody.
    let qualified = Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &select);
    let no_loader_error = qualified.as_ref().err().map(|error| error.to_string());
    assert!(
        marker.exists(),
        "no-loader qualifier must actually reach the candidate probe: {no_loader_error:?}"
    );
    std::fs::remove_file(&marker).unwrap();
    let mut recipe = qualified
        .unwrap()
        .expect("actual signed canary qualifies its private profile");
    let mut command = recipe.command(&request);
    let original = recipe.ay.take();
    assert!(recipe.configure_ay(&mut command).is_err());
    assert!(command.guard_targets().next().is_none());
    recipe.ay = original;
    recipe.configure_ay(&mut command).unwrap();
    let targets = command.guard_targets().collect::<Vec<_>>();
    assert!(recipe.configure_ay(&mut command).is_err());
    assert_eq!(command.guard_targets().collect::<Vec<_>>(), targets);
    let held = aterm_uds::deadline::Held::start(&mut command).unwrap();
    drop(command);
    drop(recipe);
    let subject = atpkg::lease::Subject::build("ay", 1).unwrap();
    assert!(
        matches!(
            atpkg::lease::reclaim(&layout.prefix, &subject),
            atpkg::lease::Reclaim::Keep(_)
        ),
        "the actual stopped child must retain the original lease without Host-side copies"
    );
    // The controlled native test image refuses the production-only argv;
    // require its actual owned exit/drain before checking reclaimability.
    let output = held
        .output_until(
            Instant::now() + Duration::from_secs(60),
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(!output.status.success());
    assert!(matches!(
        atpkg::lease::reclaim(&layout.prefix, &subject),
        atpkg::lease::Reclaim::Clear(_)
    ));
    for key in [b"LD_PRELOAD".as_slice(), b"DYLD_PRINT_LIBRARIES".as_slice()] {
        for value in [b"unqualified".as_slice(), b"".as_slice()] {
            request.env.push((key.to_vec(), value.to_vec()));
            assert!(managed_artifact_in(&layout, "ay", &program).is_some());
            let result = Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &select);
            request.env.pop();
            assert!(
                !marker.exists(),
                "an unqualified loader must not reach candidate code"
            );
            let error = result.err().unwrap();
            assert!(
                error.to_string().contains("dynamic loader overrides"),
                "{error}"
            );
        }
    }
    // A complete marker/shim and identical executable cannot substitute
    // for the signed whole payload; refuse changed non-executable data
    // before even the read-only capability subprocess.
    std::fs::write(&runtime_data, b"changed runtime data").unwrap();
    let error = Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &select)
        .err()
        .expect("changed signed payload must refuse");
    assert!(error.to_string().contains("signed tree"), "{error}");
    assert!(!marker.exists());

    // A signed package is not automatically an accepted native runtime.
    // Prove this candidate really runs and has a valid signed selection,
    // then require the actual qualifier to stop it before execution.
    std::fs::write(
        &program,
        b"#!/bin/sh\nprintf actual-script > \"$ATERM_TEST_RECIPE_PROBE_MARKER\"\n",
    )
    .unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    let script_registry = signed_tests::Registry::new(&build);
    let script_select = |layout: &atpkg::store::Layout| script_registry.acquire(layout);
    drop(script_select(&layout).expect("non-native candidate has actual signed payload authority"));
    let mut direct = spawnfd::command(&program);
    direct
        .arg("--resource-capabilities")
        .env_clear()
        .env("ATERM_TEST_RECIPE_PROBE_MARKER", &marker);
    let output = aterm_uds::deadline::output_until_complete_stdout_with_limit(
        &mut direct,
        Instant::now() + Duration::from_secs(60),
        8192,
    )
    .unwrap();
    assert!(output.status.success() && marker.exists());
    std::fs::remove_file(&marker).unwrap();
    assert!(Recipe::qualify_with_ay_selection(&request, 4, Some(&layout), &script_select).is_err());
    assert!(
        !marker.exists(),
        "signed source must still pass the native runtime profile before probing"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn an_actual_native_capability_read_survives_scheduling_delay() {
    const BODY: &str = r#"{"version":3,"cpu_workers":true,"memory_custody":"resource-inspect-v3","worker_argument":"--resource-worker-v3","subgroup_custody":"host-owned-v3","subgroup_argument":"--resource-subgroups-v3"}"#;
    let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/printf"));
    let mut command = spawnfd::command(program);
    command.args(["%s", BODY]);
    let mut capability =
        read_capability(&mut command, Instant::now() - Duration::from_secs(3)).unwrap();
    assert!(targo(&capability, &[Some("--unverified"), Some("build")]));
    capability.as_object_mut().unwrap().insert(
        "subgroup_custody".into(),
        aterm_json::Value::String("foreign".into()),
    );
    assert!(!targo(&capability, &[Some("--unverified"), Some("build")]));
    assert!(read_capability(&mut command, Instant::now() - Duration::from_secs(61)).is_none());
    let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/printf"));
    let mut malformed = spawnfd::command(program);
    malformed.args(["%s", "{"]);
    assert!(read_capability(&mut malformed, Instant::now()).is_none());
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let mut failed = spawnfd::command(program);
    failed.args(["-c", "printf '%s' \"$1\"; exit 7", "fixture", BODY]);
    assert!(read_capability(&mut failed, Instant::now()).is_none());
}
#[test]
fn malformed_metadata_purpose_cannot_fall_back_to_a_legacy_preparation_probe() {
    let mut request = Submission {
        binding: Binding {
            session: "s-a".into(),
            attempt: [1; 16],
        },
        peer_pid: std::process::id(),
        peer_birth: 1,
        argv: vec![
            b"/does-not-exist/targo".to_vec(),
            b"--resource-metadata-v1".to_vec(),
        ],
        cwd: b"/".to_vec(),
        env: Vec::new(),
        cpu_units: 2,
        memory_bytes: 1024,
        label: "closed metadata purpose".into(),
        timeout_ms: Some(600_000),
    };
    for extra in [false, true] {
        if extra {
            request.cpu_units = 1;
            request.argv.push(b"unexpected".to_vec());
        }
        let error = Recipe::qualify_in(&request, 2, None)
            .err()
            .expect("closed profile must refuse");
        assert!(
            error.to_string().contains("closed fixed-one request"),
            "refuse BEFORE executable/store selection: {error}"
        );
    }
    request.argv.pop();
    for timeout in [None, Some(600_001), Some(u64::MAX)] {
        request.timeout_ms = timeout;
        assert!(!metadata_request(&request));
        let error = Recipe::qualify_in(&request, 2, None).err().unwrap();
        assert!(error.to_string().contains("closed fixed-one request"));
    }
}
#[test]
fn original_root_discards_ancestor_jobserver_endpoints() {
    assert!(stale_fd_var(
        b"CARGO_JOBSERVER_HANDOFF",
        b"private-parent-ad"
    ));
    for name in [b"MAKEFLAGS".as_slice(), b"CARGO_MAKEFLAGS", b"MFLAGS"] {
        assert!(stale_fd_var(
            name,
            b"-j --jobserver-auth=fifo:/private/ancestor"
        ));
        assert!(!stale_fd_var(name, b"-k"));
    }
    assert!(!stale_fd_var(b"CARGO_BUILD_JOBS", b"3"));
}
#[test]
fn arbitrary_command_budget_is_not_a_qualified_memory_enforcement_recipe() {
    let pid = std::process::id();
    let request = Submission {
        binding: Binding {
            session: "s-a".into(),
            attempt: [1; 16],
        },
        peer_pid: pid,
        peer_birth: aterm_uds::process::read_birth(i32::try_from(pid).unwrap())
            .unwrap()
            .0,
        argv: vec![b"/bin/echo".to_vec(), b"unqualified".to_vec()],
        cwd: std::env::current_dir()
            .unwrap()
            .as_os_str()
            .as_bytes()
            .to_vec(),
        env: Vec::new(),
        cpu_units: 1,
        memory_bytes: 1024 * 1024,
        label: "unqualified".into(),
        timeout_ms: None,
    };
    let error = Recipe::qualify(&request, 2)
        .err()
        .expect("arbitrary argv must refuse");
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
}
