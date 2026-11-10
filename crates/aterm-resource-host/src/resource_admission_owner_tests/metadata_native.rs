// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real private manager artifact, paid Owner, native peer/SCM, production client
//! and selected no-fork image. The stand-in is this source's native test image;
//! this is not SDK/compiler business qualification or an installed publication.
use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_uds::{CtlListener, deadline, spawnfd};
use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, BorrowedFd},
    sync::Arc,
};
use trust_resource_controller::{
    self as control,
    client::{Client, MetadataIo},
};
use trust_resource_provenance::Store as ReadStore;

const ROLE: &str = "ATERM_METADATA_NATIVE_FIXTURE_ROLE";
const STORE: &str = "ATERM_METADATA_NATIVE_FIXTURE_STORE";
const CASE: &str = "ATERM_METADATA_NATIVE_FIXTURE_CASE";
const SERVICE_IMAGE: &str = "ATERM_METADATA_NATIVE_SERVICE_IMAGE";
const SERVICE_SHA256: &str = "ATERM_METADATA_NATIVE_SERVICE_SHA256";
const TARGET_SDK: &str = "ATERM_METADATA_NATIVE_TARGET_SDK";
const SERVICE_LIFETIME: Duration = Duration::from_secs(600);
const QUALIFICATION_LIFETIME: Duration = Duration::from_secs(900);
const EXACT: &str = "actor::owner::tests::metadata_native::actual_fixed_metadata_owner_preserves_original_resources_and_drains";

// These count only the owned controller's successfully emitted lifecycle
// bytes. Arbitrary target stdout never renews the native hang detector, and
// no progress reading can extend the original service absolute deadline.
fn capture_progress(path: &std::path::Path) -> u64 {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .take(256)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_bytes().ends_with(b".events"))
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}
fn store_progress(path: &std::path::Path) -> u64 {
    fs::read_dir(path.join(".ubd"))
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_bytes().starts_with(b"owner-engine-"))
        .map(|entry| capture_progress(&entry.path()))
        .sum()
}

/// This fixture's roles in the library's own test image, which is the one
/// image that calls this: from its single early entry, before libtest parses
/// argv. The harness=false member image compiles this module too, and its
/// `trustc -vV` stand-in is that image's own role, not a no-fork target here.
pub(super) fn early_role() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let operation = if args == control::METADATA_CAPABILITY_QUERY.map(OsString::from) {
        Some(0)
    } else if args == [OsString::from(control::METADATA_STARTUP_ARGUMENT)] {
        Some(1)
    } else if args == [OsString::from("-vV")]
        || args == [OsString::from("--print"), OsString::from("sysroot")]
        || args == [OsString::from("--unverified"), OsString::from("--version")]
    {
        Some(2)
    } else {
        None
    };
    if let Some(operation) = operation {
        let result = std::panic::catch_unwind(|| match operation {
            0 => {
                fs::write("metadata-capability-probed", "actual selected image ran").unwrap();
                let mut facts: aterm_json::Value =
                    aterm_json::from_str(control::METADATA_CAPABILITIES_JSON).unwrap();
                facts.as_object_mut().unwrap().insert(
                    "context_sdk".into(),
                    aterm_json::Value::String("inherited-sdk-keeper-v1".into()),
                );
                println!("{facts}");
            }
            1 => client(),
            _ => target(&args),
        });
        std::process::exit(if result.is_ok() { 0 } else { 101 });
    }
}

fn target(args: &[OsString]) {
    let mut source = Vec::new();
    std::io::stdin()
        .take(65537)
        .read_to_end(&mut source)
        .unwrap();
    assert!(
        source.is_empty(),
        "closed query target received caller source bytes"
    );
    assert!(crate::metadata_prelude::no_fork(std::process::id()).unwrap());
    assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
    assert_eq!(unsafe { libc::setsid() }, -1);
    assert_eq!(
        unsafe { libc::setpgid(0, libc::getpgid(libc::getppid())) },
        -1
    );
    for name in [
        "ATERM_RESOURCE_JOB_V3",
        "ATERM_RESOURCE_INSPECTOR",
        "ATERM_SDK_LEASE_V3",
        ROLE,
        STORE,
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "metadata target received root authority {name}"
        );
    }
    let fork = unsafe { libc::fork() };
    if fork == 0 {
        unsafe { libc::_exit(93) }
    }
    assert_eq!(
        fork, -1,
        "actual metadata target may not fork/setsid an escaped descendant"
    );
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
    if let Ok(mut child) = spawnfd::command("/usr/bin/true").spawn() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("metadata posix_spawn was allowed");
    }
    fs::write("metadata-target-started", "actual no-fork selected image").unwrap();
    if args == [OsString::from("--unverified"), OsString::from("--version")] {
        std::io::stdout()
            .write_all(&vec![b'x'; control::METADATA_MAX_OUTPUT + 32768])
            .unwrap();
    } else {
        println!("actual native metadata output");
    }
    if args != [OsString::from("-vV")] {
        loop {
            unsafe {
                libc::pause();
            }
        }
    }
}

#[test]
fn actual_fixed_metadata_owner_preserves_original_resources_and_drains() {
    if std::env::var(ROLE).as_deref() == Ok("host") {
        return host();
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("metadata-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let store_path = scratch.path().join("host");
    let store = ImageStore::prepare_at(&store_path).unwrap();
    let program = store.cache_current().unwrap();
    let mut command = spawnfd::command(program.path());
    command
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, "host")
        .env(STORE, &store_path);
    if let Some(case) = std::env::var_os(CASE) {
        command.env(CASE, case);
    }
    let output = if std::env::var(CASE).as_deref() == Ok("service") {
        deadline::output_while_progressing_until(
            &mut command,
            HANG,
            Instant::now() + QUALIFICATION_LIFETIME,
            &mut || store_progress(&store_path),
        )
    } else {
        deadline::output_within(&mut command, HANG)
    }
    .unwrap();
    if std::env::var(CASE).as_deref() == Ok("service") {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.status.success() {
        let retained = scratch.keep();
        panic!(
            "actual private metadata Owner failed; preserved {}: {output:?}",
            retained.display()
        );
    }
    assert!(
        output.status.success(),
        "actual private metadata Owner failed: {output:?}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("actual-metadata-owner-retired"));
    if std::env::var(CASE).as_deref() == Ok("service") {
        println!("retained-native-metadata-host={}", scratch.keep().display());
        return;
    }
    for entry in fs::read_dir(store_path.join("images")).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}
fn host() {
    crate::runtime::test_endpoint().unwrap();
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let store = ImageStore::open_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    let socket = path.join("m.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&socket, 0o600).unwrap();
    let _instance = store.publish(&program, &listener).unwrap();
    let mut world = World::with_scratch(Scratch::at(&path));
    let home = trust_resource_provenance::process::account_home().unwrap();
    let artifact_parent = home.join(".cache/aterm/metadata-sdk-fixtures.noindex");
    fs::create_dir_all(&artifact_parent).unwrap();
    // The strict native input profile deliberately refuses writable /tmp
    // ancestry. Only the socket/custody fixture lives there; SDK inputs use
    // an independently owned account-local tree, preserved on every failure.
    let artifacts = aterm_tempfile::Builder::new()
        .prefix("sdk-")
        .tempdir_in(&artifact_parent)
        .unwrap()
        .keep();
    eprintln!("native metadata SDK inputs: {}", artifacts.display());
    let layout = atpkg::store::Layout {
        prefix: artifacts.join("manager"),
    };
    let build = layout.build_dir("trust", 1);
    fs::create_dir_all(build.join("bin")).unwrap();
    fs::create_dir_all(build.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
    fs::create_dir_all(layout.bin_dir()).unwrap();
    let service = build.join("bin/targo");
    let mode = std::env::var(CASE).unwrap_or_else(|_| "normal".into());
    let genuine = mode == "service";
    let source = if genuine {
        PathBuf::from(
            std::env::var_os(SERVICE_IMAGE).expect("explicit frozen native service image"),
        )
    } else {
        program.path().to_path_buf()
    };
    fs::copy(source, &service).unwrap();
    atpkg::platform::install_shim_to(&layout.bin_dir().join("targo"), &service).unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    let target_layout = genuine.then(|| atpkg::store::resolve_configured().unwrap());
    let target_root = if genuine {
        std::env::var_os(TARGET_SDK)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                atpkg::ops::managed(target_layout.as_ref().unwrap(), "trustc")
                    .unwrap()
                    .target
                    .canonicalize()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_path_buf()
            })
    } else {
        artifacts.join("selected-sdk")
    };
    let target_lease = if let Some(target_layout) = &target_layout
        && atpkg::lease::Subject::of_dir(&target_layout.prefix, &target_root.join("bin")).is_some()
    {
        Some(
            owner_fixture_sdk_lease(
                &target_layout.prefix,
                &target_root.join("bin"),
                "actual bounded native metadata qualification",
                Instant::now(),
            )
            .unwrap(),
        )
    } else {
        None
    };
    let selected = if genuine {
        target_root.join("bin/trustc")
    } else {
        fs::create_dir_all(target_root.join("bin")).unwrap();
        fs::create_dir_all(target_root.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
        let selected = target_root.join("bin/native-metadata");
        fs::copy(program.path(), &selected).unwrap();
        selected
    };
    // Diagnostic-only: retain the handshake to isolate the frozen old client's
    // wrong cancellation channel from its independent closed-peer pin defect.
    // Production always closes after ACK and final green uses that exact path.
    let retain_control = mode.ends_with("-isolated");
    let target_mode = if mode == "drop-preparing" {
        "normal"
    } else {
        mode.strip_suffix("-isolated").unwrap_or(&mode)
    };
    let plan = aterm_json::json!({"store":path,"root":target_root,"program":selected,"cwd":world.scratch.path,"mode":target_mode});
    let bytes = plan.to_string().into_bytes();
    let batch = genuine.then(|| {
        let mut sha256 = [0; 32];
        assert!(aterm_codec::hex::decode_into(
            std::env::var(SERVICE_SHA256).unwrap().as_bytes(),
            &mut sha256
        ));
        assert_eq!(
            aterm_digest::Sha256::digest(fs::read(&service).unwrap()),
            sha256
        );
        control::MetadataBatch {
            host_store: Some(path.as_os_str().as_bytes().to_vec()),
            service_program: service.as_os_str().as_bytes().to_vec(),
            service_sha256: sha256,
            probes: [
                control::MetadataProbeCommand::VerboseVersion,
                control::MetadataProbeCommand::Sysroot,
                control::MetadataProbeCommand::UnverifiedVersion,
            ]
            .into_iter()
            .map(|command| {
                let program = if command == control::MetadataProbeCommand::UnverifiedVersion {
                    target_root.join("bin/targo")
                } else {
                    selected.clone()
                };
                control::MetadataProbe {
                    root: target_root.as_os_str().as_bytes().to_vec(),
                    program: program.as_os_str().as_bytes().to_vec(),
                    program_sha256: aterm_digest::Sha256::digest(fs::read(program).unwrap()),
                    command,
                    cwd: target_root.as_os_str().as_bytes().to_vec(),
                }
            })
            .collect(),
        }
    });
    let (reader, mut input): (std::os::fd::OwnedFd, _) = if let Some(batch) = &batch {
        let filename = world.scratch.path.join("metadata-batch.input");
        fs::write(&filename, batch.encode().unwrap()).unwrap();
        (File::open(filename).unwrap().into(), None)
    } else {
        let (reader, mut input) = io::pipe().unwrap();
        input
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(&bytes).unwrap();
        (reader.into(), Some(input))
    };
    let (binding, memory, events) = world.queue_sleep(HANG);
    let pending = world.owner.pending.back_mut().unwrap();
    if genuine {
        pending.submission.request.timeout_ms = Some(SERVICE_LIFETIME.as_millis() as u64);
    } else if mode == "no-ready" {
        pending.submission.request.timeout_ms = Some(2000);
    }
    pending.submission.request.argv = vec![
        service.as_os_str().as_bytes().to_vec(),
        control::METADATA_STARTUP_ARGUMENT.as_bytes().to_vec(),
    ];
    pending.submission.request.env = vec![
        (b"LANG".to_vec(), b"C".to_vec()),
        (b"LC_ALL".to_vec(), b"C".to_vec()),
    ];
    for name in [
        b"DYLD_INSERT_LIBRARIES".as_slice(),
        b"DYLD_LIBRARY_PATH",
        b"RUSTC_WRAPPER",
        b"CLANG_CONFIG_SYSTEM_DIR",
        b"ATERM_RESOURCE_JOB_V3",
    ] {
        pending
            .submission
            .request
            .env
            .push((name.to_vec(), b"caller-controlled".to_vec()));
        let error = recipe::Recipe::qualify_in(&pending.submission.request, 2, Some(&layout))
            .err()
            .expect("unmanaged environment must refuse before capability code");
        assert!(error.to_string().contains("closed native profile"));
        assert!(
            !world
                .scratch
                .path
                .join("metadata-capability-probed")
                .exists()
        );
        assert!(matches!(
            atpkg::lease::holders(
                &layout.prefix,
                &atpkg::lease::Subject::build("trust", 1).unwrap()
            ),
            atpkg::lease::Holders::Free
        ));
        pending.submission.request.env.pop();
    }
    pending.submission.fds.stdin = reader;
    pending.submission.fds.stderr = fs::File::create(world.scratch.path.join("root.stderr"))
        .unwrap()
        .into();
    pending.submission.fds.stdout = fs::File::create(world.scratch.path.join("root.stdout"))
        .unwrap()
        .into();
    pending.recipe = None;
    world.owner.recipe_layout = Some(layout.clone());
    let mut paused = if mode == "drop-preparing" {
        let (pause, control) = preparation::Pause::new();
        world.owner.metadata_pause = Some(pause);
        Some(control)
    } else {
        None
    };
    assert_eq!(read_event(&events, &binding), Event::Queued);
    world.owner.tick_queue();
    assert!(
        world.owner.pending.front().unwrap().admitted.is_some(),
        "expensive root recipe is inside actual paid CPU/memory"
    );
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    let until = Instant::now()
        + if genuine {
            QUALIFICATION_LIFETIME
        } else {
            HANG
        };
    let mut last_progress = (capture_progress(&world.scratch.path), Instant::now());
    let mut member_seen = false;
    let mut sdk_seen = false;
    let mut sdk_loaded_checked = false;
    let mut root_pid = None;
    let mut released = false;
    let mut controls = Vec::new();
    while !world.owner.idle() {
        let progress = capture_progress(&world.scratch.path);
        if progress != last_progress.0 {
            last_progress = (progress, Instant::now());
        }
        assert!(
            last_progress.1.elapsed() < HANG,
            "native lifecycle stopped progressing while original custody remained held"
        );
        if let Some(lease) = &target_lease {
            lease.check_current().unwrap();
        }
        world.owner.tick();
        if paused
            .as_ref()
            .is_some_and(|control| control.entered.try_recv().is_ok())
        {
            drop_preparing(
                &mut world,
                &binding,
                &memory,
                &layout,
                paused.take().unwrap(),
            );
            println!("actual-metadata-owner-retired mode={mode} cpu=0 memory=closed SDK=free");
            fs::remove_file(socket).unwrap();
            fs::remove_dir_all(artifacts).unwrap();
            return;
        }
        if let Some(job) = world
            .owner
            .jobs
            .get(&(binding.session.clone(), binding.attempt))
        {
            root_pid = Some(u32::try_from(job.child.cohort().group()).unwrap());
            if !job.members.is_empty() {
                member_seen = true;
                assert_eq!(world.owner.debt.reserved().unwrap(), 1);
                assert!(job.metadata_sdk.is_some());
            }
            if let Some(sdk) = &job.metadata_sdk {
                sdk_seen = true;
                if job.metadata_ready && !sdk_loaded_checked {
                    let checked = sdk.validate_loaded_at(
                        root_pid.unwrap(),
                        Instant::now() - Duration::from_secs(3),
                    );
                    assert!(checked.is_ok(), "original loaded SDK: {checked:?}");
                    assert!(
                        sdk.validate_loaded_at(root_pid.unwrap(), Instant::now() - HANG)
                            .is_err()
                    );
                    sdk_loaded_checked = true;
                }
                assert!(matches!(
                    atpkg::lease::holders(
                        &layout.prefix,
                        &atpkg::lease::Subject::build("trust", 1).unwrap()
                    ),
                    atpkg::lease::Holders::Held(_)
                ));
            }
        }
        match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => {
                let end = crate::fixture_deadline(Instant::now());
                let peer = match wire::local_peer(&stream) {
                    Ok(peer) => peer,
                    Err(error) if error.kind() == io::ErrorKind::NotConnected => continue,
                    Err(error) => panic!("actual metadata native peer: {error}"),
                };
                match crate::runtime::read_json(&stream, end) {
                    Ok(hello) if hello["role"].as_str() == Some("native-metadata-ready-v1") => {
                        let result = crate::resource_context::serve_metadata_ready(
                            &stream,
                            peer,
                            &hello,
                            end,
                            |peer, query| world.owner.metadata_ready(peer, &query),
                        );
                        if let Err(error) = result {
                            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                            assert!(world.owner.jobs.values().all(|job| !job.metadata_ready));
                        }
                    }
                    Ok(hello) if hello["role"].as_str() == Some("native-context-sdk-v1") => {
                        crate::resource_context::serve_sdk(
                            &stream,
                            peer,
                            &hello,
                            end,
                            |peer, query| world.owner.context(peer, &query),
                        )
                        .unwrap();
                    }
                    Ok(hello) if hello["role"].as_str() == Some("native-context") => {
                        crate::resource_context::serve(
                            &stream,
                            peer,
                            &hello,
                            end,
                            |peer, query| world.owner.context(peer, &query),
                        )
                        .unwrap();
                    }
                    Ok(hello) => {
                        let selected = members::binding_role(&hello, "sealed-metadata-v1").unwrap();
                        wire::write_frame(&stream, aterm_types::resource_subgroup::PREPARE, end)
                            .unwrap();
                        let incoming = crate::metadata_transport::receive_authorized(
                            &stream,
                            &selected,
                            end,
                            |request, peer| {
                                let mut check = members::Authorization::of(request, peer).unwrap();
                                check.metadata = true;
                                world.owner.authorize_member(&check)
                            },
                        );
                        let incoming = match incoming {
                            Ok(incoming) => incoming,
                            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                                assert!(world.owner.jobs.values().all(|job| !job.metadata_ready));
                                assert!(
                                    world.owner.jobs.values().all(|job| job.members.is_empty())
                                );
                                continue;
                            }
                            Err(error) => panic!("actual metadata attachment: {error}"),
                        };
                        let member = incoming.sealed.member.member_attempt;
                        let stem = format!("member-{}", aterm_codec::hex::encode_lower(&member));
                        if genuine {
                            fs::write(
                                world.scratch.path.join(format!("{stem}.request12")),
                                &incoming.request_bytes,
                            )
                            .unwrap();
                        }
                        world.owner.submit_metadata(incoming);
                        if genuine {
                            world
                                .owner
                                .jobs
                                .get_mut(&(selected.session.clone(), selected.attempt))
                                .unwrap()
                                .members
                                .get_mut(&member)
                                .unwrap()
                                .audit_to(
                                    File::create(world.scratch.path.join(format!("{stem}.events")))
                                        .unwrap(),
                                );
                        }
                        aterm_ctl::resource_subgroup_client::acknowledge(&stream, end).unwrap();
                        if retain_control {
                            controls.push(stream);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
                    Err(error) => panic!("actual metadata connection: {error}"),
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("actual metadata listener: {error}"),
        }
        if !released
            && world
                .scratch
                .path
                .join("metadata-client-complete")
                .is_file()
        {
            assert_eq!(
                world.owner.debt.reserved().unwrap(),
                1,
                "member completion does not release original root CPU"
            );
            assert!(
                world.owner.jobs.values().all(|job| job.members.is_empty()),
                "client completion requires actual event EOF after scope retirement"
            );
            released = true;
            input.take();
        }
        assert!(
            Instant::now() < until,
            "actual metadata owner must drain all original resources"
        );
        if !world.owner.idle() {
            std::thread::sleep(POLL);
        }
    }
    let root_started = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    let Event::Started { pid, .. } = EventFrame::decode(&root_started, &binding).unwrap().event
    else {
        panic!("metadata root never Started")
    };
    let root_finished = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    let Event::Finished { outcome, cause, .. } =
        EventFrame::decode(&root_finished, &binding).unwrap().event
    else {
        panic!("metadata root has no actual exit")
    };
    if genuine {
        fs::write(world.scratch.path.join("root-started.frame"), root_started).unwrap();
        fs::write(
            world.scratch.path.join("root-finished.frame"),
            root_finished,
        )
        .unwrap();
    }
    if mode == "no-ready" {
        assert!(matches!(outcome, Outcome::Signal(_)));
        assert_eq!(
            cause,
            aterm_types::resource_admission::CompletionCause::Timeout
        );
        assert!(world.scratch.path.join("metadata-root-no-ready").is_file());
        assert!(!world.scratch.path.join("metadata-target-started").exists());
        assert!(!member_seen && sdk_seen);
        world.assert_settled(&binding, &memory, pid);
        assert!(matches!(
            atpkg::lease::holders(
                &layout.prefix,
                &atpkg::lease::Subject::build("trust", 1).unwrap()
            ),
            atpkg::lease::Holders::Free
        ));
        println!("actual-metadata-owner-retired mode={mode} cpu=0 memory=closed SDK=free");
        fs::remove_file(socket).unwrap();
        fs::remove_dir_all(artifacts).unwrap();
        return;
    }
    assert_eq!(
        outcome,
        Outcome::Exit(0),
        "actual root stdout={} stderr={}",
        String::from_utf8_lossy(&fs::read(world.scratch.path.join("root.stdout")).unwrap()),
        fs::read_to_string(world.scratch.path.join("root.stderr")).unwrap()
    );
    assert_eq!(root_pid, Some(pid));
    assert!(member_seen && sdk_seen && (released || genuine));
    if mode == "normal" {
        assert!(sdk_loaded_checked);
    }
    assert!(
        !world
            .scratch
            .path
            .join("metadata-capability-probed")
            .exists(),
        "no selected code may run in an unregistered capability subprocess"
    );
    world.assert_settled(&binding, &memory, pid);
    if let Some(batch) = &batch {
        let output = world.scratch.path.join("root.stdout");
        assert!(fs::metadata(&output).unwrap().len() <= control::MAX_PAYLOAD as u64);
        let report =
            control::MetadataBatchReport::decode(&fs::read(output).unwrap(), batch).unwrap();
        assert!(
            report.failure.is_none(),
            "actual native service failure: {:?}",
            report.failure
        );
        assert_eq!(report.records.len(), 3);
        let runner = report.runner.unwrap();
        assert_eq!(runner.identity.pid, pid);
        assert_eq!(runner.executable.sha256, batch.service_sha256);
        for record in &report.records {
            assert!(record.completion.inputs_unchanged.is_ok());
            assert!(!record.output_limit_exceeded);
            assert!(matches!(
                record.completion.finished.status,
                control::Status::Finished {
                    outcome: control::Outcome::Exit(0),
                    cause: control::CompletionCause::Exited,
                    ..
                }
            ));
            println!(
                "actual-sdk-metadata program={} stdout={}",
                String::from_utf8_lossy(&record.prepared.executable.path),
                String::from_utf8_lossy(&record.stdout)
            );
        }
        if let Some(lease) = &target_lease {
            lease.check_current().unwrap();
        }
        println!(
            "actual-service-runner pid={pid} target={} records=3 target-atpkg-lease={} original-cpu=0 original-memory=closed",
            target_root.display(),
            target_lease.is_some()
        );
    }
    assert!(matches!(
        atpkg::lease::holders(
            &layout.prefix,
            &atpkg::lease::Subject::build("trust", 1).unwrap()
        ),
        atpkg::lease::Holders::Free
    ));
    println!("actual-metadata-owner-retired mode={mode} cpu=0 memory=closed SDK=free");
    if genuine {
        let selected = aterm_json::json!({"target_root":target_root,
            "expected_target_commit":std::env::var("ATERM_METADATA_NATIVE_EXPECTED_TARGET_COMMIT").ok(),
            "service_sha256":std::env::var(SERVICE_SHA256).unwrap(),
            "scope":"actual native metadata; no promoter seal, stage2, compiler-build or pair qualification"});
        fs::write(
            world.scratch.path.join("selected-inputs.json"),
            selected.to_string(),
        )
        .unwrap();
        let capture = artifacts.join("capture");
        fs::create_dir(&capture).unwrap();
        for entry in fs::read_dir(&world.scratch.path).unwrap() {
            let source = entry.unwrap().path();
            let name = source.file_name().unwrap().to_str().unwrap();
            if source.is_file()
                && (name.starts_with("member-")
                    || matches!(
                        name,
                        "metadata-batch.input"
                            | "root.stdout"
                            | "root.stderr"
                            | "root-started.frame"
                            | "root-finished.frame"
                            | "selected-inputs.json"
                    ))
            {
                let target = capture.join(name);
                fs::copy(&source, &target).unwrap();
                let bytes = fs::read(&target).unwrap();
                assert_eq!(bytes, fs::read(&source).unwrap());
                fs::set_permissions(&target, fs::Permissions::from_mode(0o400)).unwrap();
                println!(
                    "capture-file={} bytes={} sha256={}",
                    target.display(),
                    bytes.len(),
                    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes))
                );
            }
        }
        println!("stable-native-metadata-capture={}", capture.display());
        println!(
            "retained-native-metadata-capture={}",
            world.scratch.path.display()
        );
        println!("retained-native-service-artifact={}", artifacts.display());
        world.scratch.preserve = true;
        return;
    }
    fs::remove_file(socket).unwrap();
    fs::remove_dir_all(artifacts).unwrap();
}

fn drop_preparing(
    world: &mut World,
    binding: &Binding,
    memory: &CohortClosureRef,
    layout: &atpkg::store::Layout,
    control: preparation::Paused,
) {
    let mut job = world
        .owner
        .jobs
        .remove(&(binding.session.clone(), binding.attempt))
        .unwrap();
    let pid = job.child.child_mut().id();
    assert!(job.metadata_sdk.is_some());
    job.child
        .request_group(aterm_capacity::cohort::launch::CohortSignal::Kill)
        .unwrap();
    assert!(!job.child.child_mut().wait().unwrap().success());
    // Root's original slot5 and its early duplicate are now actually closed.
    // The sole service SDK owner must survive the member's real Drop/join.
    let prefix = layout.prefix.clone();
    let debt = world.owner.debt.clone();
    let ledger = Ledger::at(world.namespace.fence());
    let reference = memory.clone();
    let observer = std::thread::spawn(move || {
        control.dropping.recv_timeout(HANG).unwrap();
        let busy = matches!(
            atpkg::lease::reclaim(&prefix, &atpkg::lease::Subject::build("trust", 1).unwrap()),
            atpkg::lease::Reclaim::Keep(atpkg::lease::Holders::Held(_))
        );
        let cpu = debt.reserved().unwrap();
        let memory = ledger.cohort_claim_state(&reference).unwrap();
        control.release.send(()).unwrap();
        (busy, cpu, memory)
    });
    drop(job);
    let observed = observer.join().unwrap();
    world.owner.recover().unwrap();
    world.settle();
    world.assert_settled(binding, memory, pid);
    aterm_uds::deadline::once_released(|| {
        match atpkg::lease::reclaim(
            &layout.prefix,
            &atpkg::lease::Subject::build("trust", 1).unwrap(),
        ) {
            atpkg::lease::Reclaim::Clear(guard) => Ok(guard),
            atpkg::lease::Reclaim::Keep(atpkg::lease::Holders::Held(_)) => {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            other => panic!("settled SDK cannot be reclaimed: {other:?}"),
        }
    });
    assert_eq!(
        observed,
        (true, 1, CohortClaimState::Retained),
        "original SDK/CPU/memory must remain owned at the real paused-worker join boundary after root death"
    );
}

fn client() {
    let pending = control::client::sdk::snapshot_sdk_lease().unwrap();
    // The actual owned service receives the original GNU pair at3/4. This
    // fixed import duplicates only those occupied descriptors and creates no tokens.
    let descriptions = unsafe {
        [
            BorrowedFd::borrow_raw(3).try_clone_to_owned().unwrap(),
            BorrowedFd::borrow_raw(4).try_clone_to_owned().unwrap(),
        ]
    };
    unsafe {
        control::client::sdk::capture_sdk_lease(pending).unwrap();
    }
    let mut prefix = [0; 4];
    std::io::stdin().read_exact(&mut prefix).unwrap();
    let length = u32::from_be_bytes(prefix) as usize;
    assert!(length <= 65536);
    let mut bytes = vec![0; length];
    std::io::stdin().read_exact(&mut bytes).unwrap();
    let plan: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    if plan["mode"].as_str() == Some("no-ready") {
        fs::write(
            std::path::Path::new(plan["cwd"].as_str().unwrap()).join("metadata-root-no-ready"),
            "actual root ran without readiness",
        )
        .unwrap();
        loop {
            unsafe {
                libc::pause();
            }
        }
    }
    let until = Instant::now() + HANG;
    let selected = ReadStore::open_at(std::path::Path::new(plan["store"].as_str().unwrap()))
        .unwrap()
        .connect_ancestor(until)
        .unwrap();
    let pin = Arc::new(selected.into_helper(until).unwrap());
    let caller = Client::from_helper(pin.clone());
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let reply = caller
        .context_with_sdk(
            &control::ContextQuery {
                binding: binding.clone(),
                call_attempt: [65; 16],
            },
            until,
        )
        .unwrap();
    assert_eq!(reply.context.original_root.pid, std::process::id());
    assert!(matches!(
        reply.context.cpu,
        control::Cpu::RetainedFixed { units: 1, .. }
    ));
    control::client::sdk::validate_sdk_keeper(&reply.context).unwrap();
    let unready = pin.connect(until).unwrap();
    let mut channel = control::Channel::new(unready.stream().try_clone().unwrap()).unwrap();
    let mut facts: aterm_json::Value =
        aterm_json::from_str(control::METADATA_CAPABILITIES_JSON).unwrap();
    facts
        .as_object_mut()
        .unwrap()
        .insert("metadata_role".into(), aterm_json::Value::from(2u64));
    let hello = aterm_json::json!({"version":3,"role":"native-metadata-ready-v1","session":binding.session,"capabilities":facts});
    channel
        .send(&aterm_json::to_vec(&hello).unwrap(), until)
        .unwrap();
    assert!(
        channel.receive(until).is_err(),
        "old conflicting wire role cannot negotiate readiness"
    );
    drop((channel, unready));
    let program = PathBuf::from(plan["program"].as_str().unwrap());
    let digest = aterm_digest::Sha256::digest(fs::read(&program).unwrap());
    let mode = plan["mode"].as_str().unwrap();
    let args = match mode {
        "normal" => vec![b"-vV".to_vec()],
        "cancel" => vec![b"--print".to_vec(), b"sysroot".to_vec()],
        "overflow" => vec![b"--unverified".to_vec(), b"--version".to_vec()],
        _ => panic!("unknown native fixture case"),
    };
    let request = control::SealedMetadataRequest {
        member: control::Request {
            binding,
            member_attempt: [66; 16],
            role: control::Role::SealedMetadata,
            command: control::CommandSpec {
                program: program.as_os_str().as_bytes().to_vec(),
                arg0: None,
                args,
                environment: vec![
                    (b"LANG".to_vec(), b"C".to_vec()),
                    (b"LC_ALL".to_vec(), b"C".to_vec()),
                ],
                cwd: plan["cwd"].as_str().unwrap().as_bytes().to_vec(),
            },
            delegations: control::Delegations {
                cpu: None,
                gnu: control::GnuTransport::NumericPipe {
                    read: control::Descriptor {
                        source: descriptions[0].as_raw_fd() as u32,
                        target: 3,
                    },
                    write: control::Descriptor {
                        source: descriptions[1].as_raw_fd() as u32,
                        target: 4,
                    },
                },
                fences: Vec::new(),
            },
            timeout_ns: Some(if mode.ends_with("-isolated") {
                2_000_000_000
            } else {
                control::METADATA_MAX_TIMEOUT_NS
            }),
        },
        cpu: control::MetadataCpu::FixedBorrow,
        root: plan["root"].as_str().unwrap().as_bytes().to_vec(),
        program_sha256: digest,
    };
    let denied_fds = descriptions
        .iter()
        .map(|fd| fd.try_clone().unwrap())
        .collect::<Vec<_>>();
    let mut denied = request.clone();
    denied.member.delegations.gnu = control::GnuTransport::NumericPipe {
        read: control::Descriptor {
            source: denied_fds[0].as_raw_fd() as u32,
            target: 3,
        },
        write: control::Descriptor {
            source: denied_fds[1].as_raw_fd() as u32,
            target: 4,
        },
    };
    assert!(
        matches!(
            caller.metadata(
                &denied,
                MetadataIo {
                    descriptions: denied_fds
                },
                until
            ),
            Err(control::client::MetadataError::Refused)
        ),
        "generic SDK context cannot authorize a metadata member"
    );
    let ready_query = control::ContextQuery {
        binding: request.member.binding.clone(),
        call_attempt: [67; 16],
    };
    let ready = caller.metadata_context(&ready_query, until).unwrap();
    control::client::sdk::validate_sdk_keeper(&ready.context).unwrap();
    assert!(
        caller
            .metadata_context(
                &control::ContextQuery {
                    call_attempt: [68; 16],
                    ..ready_query
                },
                until
            )
            .is_err(),
        "original root readiness is one-use"
    );
    let mut member = caller
        .metadata(
            &request,
            MetadataIo {
                descriptions: Vec::from(descriptions),
            },
            until,
        )
        .unwrap();
    let mut cancelled = false;
    let result = loop {
        if let Some(result) = member.poll(until).unwrap() {
            break result;
        }
        if mode == "cancel" && !cancelled && member.started().is_some() {
            member.cancel(until).unwrap();
            cancelled = true;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(result.completion.inputs_unchanged.is_ok());
    assert_eq!(result.prepared.root, request.root);
    assert!(result.stdout.len() + result.stderr.len() <= control::METADATA_MAX_OUTPUT);
    match mode {
        "normal" => assert!(matches!(
            result.completion.finished.status,
            control::Status::Finished {
                outcome: control::Outcome::Exit(0),
                cause: control::CompletionCause::Exited,
                ..
            }
        )),
        "cancel" => {
            assert!(cancelled && !result.output_limit_exceeded);
            assert!(
                matches!(
                    result.completion.finished.status,
                    control::Status::Finished {
                        cause: control::CompletionCause::Cancelled,
                        ..
                    }
                ),
                "requesting cancellation is not a Cancelled outcome: {:?}",
                result.completion.finished.status
            );
        }
        "overflow" => {
            assert!(result.output_limit_exceeded);
            assert!(
                matches!(
                    result.completion.finished.status,
                    control::Status::Finished {
                        cause: control::CompletionCause::Cancelled,
                        ..
                    }
                ),
                "overflow cancellation did not reach actual owner: {:?}",
                result.completion.finished.status
            );
        }
        _ => unreachable!(),
    }
    fs::write(
        std::path::Path::new(plan["cwd"].as_str().unwrap()).join("metadata-client-complete"),
        "actual completion plus clean EOF",
    )
    .unwrap();
    let mut byte = [0];
    assert_eq!(std::io::stdin().read(&mut byte).unwrap(), 0);
}
