// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual cached host, owned root, canonical client and production RPC router.
//! Peer, capability, original claim identity and Owner callback are real. World
//! supplies controlled private admission headroom; this is no machine-pressure test.

use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_toolchain::record_lock::RecordLock;
use aterm_uds::{CtlListener, deadline, spawnfd};
use std::fs;
use std::os::fd::AsFd;
use trust_resource_controller::{self as control, client::Client};
use trust_resource_provenance::Store as ReadStore;

const ROLE: &str = "ATERM_CONTEXT_RPC_FIXTURE_ROLE";
const STORE: &str = "ATERM_CONTEXT_RPC_FIXTURE_STORE";
const EXACT: &str = "actor::owner::tests::context_rpc::actual_native_context_rpc_uses_original_owner_and_pinned_host";

#[test]
fn actual_native_context_rpc_uses_original_owner_and_pinned_host() {
    match std::env::var(ROLE).as_deref() {
        Ok("host") => return host(),
        Ok("client") => return client(),
        _ => {}
    }
    // Socket tests need a short PRIVATE directory even if the native verifier
    // supplies a long TMPDIR. No build target or canonical account state lives here.
    let scratch = aterm_tempfile::Builder::new()
        .prefix("ctx-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let path = scratch.path().join("store");
    let store = ImageStore::prepare_at(&path).unwrap();
    aterm_capacity::host_executable::narrow_test_image();
    let program = store.cache_current().unwrap();
    let mut command = spawnfd::command(program.path());
    command
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, "host")
        .env(STORE, &path);
    let output = deadline::output_within(&mut command, HANG).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output
            .stdout
            .windows(18)
            .any(|v| v == b"real-context-owner")
    );
    // Only our private cache, after the actual host/root have ended. Restore
    // directory write permission so TempDir can remove immutable test copies.
    for entry in fs::read_dir(path.join("images")).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn host() {
    println!("real-context-owner");
    crate::runtime::test_endpoint().unwrap();
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let store = ImageStore::open_at(&path).unwrap();
    aterm_capacity::host_executable::narrow_test_image();
    let program = store.cache_current().unwrap();
    let socket = path.join("h.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&socket, 0o600).unwrap();
    let _instance = store.publish(&program, &listener).unwrap();

    let mut world = World::with_scratch(Scratch::at(&path));
    let (binding, memory, events) = world.queue_sleep(HANG);
    let pending = world.owner.pending.back_mut().unwrap();
    let arguments = vec![
        OsString::from("--exact"),
        EXACT.into(),
        "--nocapture".into(),
    ];
    let environment = vec![
        (ROLE.into(), "client".into()),
        (STORE.into(), path.as_os_str().to_owned()),
    ];
    pending.submission.request.argv =
        std::iter::once(program.path().as_os_str().as_bytes().to_vec())
            .chain(arguments.iter().map(|v| v.as_os_str().as_bytes().to_vec()))
            .collect();
    pending.recipe = Some(recipe::Recipe {
        kind: recipe::Kind::Ay,
        compiler: None,
        program: program.path().to_owned(),
        arguments,
        environment,
        cwd: world.scratch.path.clone(),
        width: 1,
        metadata_sdk: None,
    });
    assert_eq!(read_event(&events, &binding), Event::Queued);
    world.owner.tick_queue();
    let Event::Started { pid, birth, .. } = read_event(&events, &binding) else {
        panic!("actual context root did not start")
    };
    assert!(pid > 1 && birth > 0);
    let until = Instant::now() + HANG;
    let mut lookups = 0;
    while !world.owner.idle() {
        world.owner.tick();
        match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => {
                let end =
                    crate::runtime::control_deadline(Instant::now() - Duration::from_secs(11));
                let peer = match wire::local_peer(&stream) {
                    Ok(peer) => peer,
                    Err(error) if error.kind() == io::ErrorKind::NotConnected => continue,
                    Err(error) => panic!("actual native context peer: {error}"),
                };
                // Selection first authenticates a peer then closes pre-HELLO;
                // that EOF must not fault the actor or change any resources.
                match crate::runtime::read_json(&stream, end) {
                    Ok(hello) => {
                        assert_eq!(peer.pid, pid);
                        assert_eq!(peer.birth, birth);
                        let held = world.owner.debt.reserved().unwrap();
                        assert_eq!(held, 1);
                        match hello["role"].as_str() {
                            Some("native-context") => crate::resource_context::serve(
                                &stream,
                                peer,
                                &hello,
                                end,
                                |peer, query| world.owner.context(peer, &query),
                            ),
                            Some(control::runtime_input_retention::SUPPORT_ROLE) => {
                                crate::runtime_input_transport::support_with(
                                    &stream,
                                    peer,
                                    &hello,
                                    end,
                                    |peer, query| world.owner.context(peer, &query),
                                )
                            }
                            Some(control::runtime_input_retention::PURPOSE) => {
                                let owner = std::cell::RefCell::new(&mut world.owner);
                                crate::runtime_input_transport::serve_with(
                                    &stream,
                                    peer,
                                    &hello,
                                    end,
                                    |peer, request| {
                                        owner.borrow_mut().runtime_inputs_begin(peer, request)
                                    },
                                    |peer, query| owner.borrow_mut().context(peer, &query),
                                    |preparing, descriptions, transport| {
                                        owner.borrow_mut().runtime_inputs_retain(
                                            preparing,
                                            descriptions,
                                            transport,
                                        )
                                    },
                                )
                            }
                            other => panic!("unexpected actual native context role {other:?}"),
                        }
                        .unwrap();
                        assert_eq!(world.owner.debt.reserved().unwrap(), held);
                        lookups += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
                    Err(error) => panic!("actual native context framing: {error}"),
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("actual native context listener: {error}"),
        }
        assert!(
            Instant::now() < until,
            "actual context root failed to retire"
        );
        if !world.owner.idle() {
            std::thread::sleep(POLL);
        }
    }
    assert_eq!(lookups, 4);
    let Event::Finished { outcome, .. } = read_event(&events, &binding) else {
        panic!("actual context root supplied no terminal fact")
    };
    assert_eq!(outcome, Outcome::Exit(0));
    world.assert_settled(&binding, &memory, pid);
    // The client closed every original input and its observation owner while
    // the Host was still active. Only actual original settlement closes the
    // Host's shared output-lock copy.
    let released = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("runtime-output/lock"))
        .unwrap();
    drop(aterm_uds::deadline::once_released(|| {
        RecordLock::try_lock(released.try_clone()?, 0).map_err(std::io::Error::from)
    }));
    fs::remove_file(socket).unwrap();
}

fn client() {
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let until = Instant::now() + HANG;
    let selected = ReadStore::open_at(&path)
        .unwrap()
        .connect_ancestor(until)
        .unwrap();
    let caller = Client::from_helper(std::sync::Arc::new(selected.into_helper(until).unwrap()));
    // The real Owner inserted this private context before the actual root GO.
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let mut query = control::ContextQuery {
        binding,
        call_attempt: [61; 16],
    };
    let original_query = query.clone();
    let reply = caller.context(&query, until).unwrap();
    let pid = std::process::id();
    let birth = process::read_birth(i32::try_from(pid).unwrap()).unwrap().0;
    assert_eq!(reply.context.original_root.pid, pid);
    assert_eq!(reply.context.original_root.birth, birth);
    assert_eq!(reply.context.memory.bytes, MEMORY);
    assert!(
        !reply
            .context
            .may_acquire_initial_cpu(reply.context.original_root)
    );
    assert!(matches!(
        reply.context.cpu,
        control::Cpu::RetainedFixed { units: 1, .. }
    ));
    query.call_attempt = [62; 16];
    query.binding.capability[0] ^= 1;
    let denied = caller.context(&query, until).unwrap_err();
    assert_eq!(
        denied.delivery,
        control::client::Delivery::ContextUnavailable
    );
    runtime_input_handoff(&caller, &path, original_query, until);
}

fn runtime_input_handoff(
    caller: &Client,
    path: &std::path::Path,
    mut query: control::ContextQuery,
    until: Instant,
) {
    use control::runtime_input_retention as inputs;
    query.call_attempt = [63; 16];
    let support = caller
        .runtime_input_support(
            &inputs::SupportQuery {
                query: query.clone(),
            },
            until,
        )
        .unwrap();
    assert!(support.observation().context.sdk.is_none());
    let runtime = path.join("runtime-inputs");
    let output = path.join("runtime-output");
    fs::create_dir(&runtime).unwrap();
    fs::create_dir(&output).unwrap();
    let mut observed = vec![super::runtime_inputs::input(
        inputs::Kind::RuntimeRoot,
        &runtime,
        b"",
    )];
    let mut originals = vec![File::open(&runtime).unwrap()];
    for index in 0..12 {
        let logical = format!("d{index}");
        let directory = runtime.join(&logical);
        fs::create_dir(&directory).unwrap();
        observed.push(super::runtime_inputs::input(
            inputs::Kind::RuntimeDirectory,
            &directory,
            logical.as_bytes(),
        ));
        originals.push(File::open(directory).unwrap());
    }
    for index in 0..156 {
        let logical = format!("d{}/input{index}", index % 12);
        let file = runtime.join(&logical);
        fs::write(&file, format!("actual input {index}")).unwrap();
        observed.push(super::runtime_inputs::input(
            inputs::Kind::RuntimeFile,
            &file,
            logical.as_bytes(),
        ));
        originals.push(File::open(file).unwrap());
    }
    observed.push(super::runtime_inputs::input(
        inputs::Kind::OutputRoot,
        &output,
        b"",
    ));
    originals.push(File::open(&output).unwrap());
    let lock_path = output.join("lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .unwrap();
    // Explicitly convert the SAME actual lock to close-only custody before
    // offering a copy. Ordinary LOCK_UN Drop would unlock the Host's copy.
    let lock = RecordLock::try_lock(lock, 0).unwrap().into_close_only();
    observed.push(super::runtime_inputs::input(
        inputs::Kind::OutputLock,
        &lock_path,
        b"lock",
    ));
    originals.push(File::from(lock.try_clone_fd().unwrap()));
    drop(lock);
    assert_eq!(observed.len(), 171);
    assert_eq!(originals.len(), 171);
    query.call_attempt = [64; 16];
    let request = inputs::Request {
        query,
        transfer: [65; 16],
        inputs: observed,
    };
    let retained = caller
        .retain_runtime_inputs(
            &request,
            &originals.iter().map(AsFd::as_fd).collect::<Vec<_>>(),
            until,
        )
        .unwrap();
    assert_eq!(retained.request().inputs.len(), 171);
    assert!(retained.observation().context.sdk.is_none());
    drop(originals);
    drop(retained);
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(
        matches!(
            RecordLock::try_lock(contender, 0),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "closing original inputs and Retained observation released Host custody before root settlement"
    );
}
