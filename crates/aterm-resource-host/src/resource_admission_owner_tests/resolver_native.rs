// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real native custody with a selected stand-in image. This exercises the host
//! protocol and confinement; it does not qualify Targo's resolver semantics.
use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_uds::{CtlListener, deadline, spawnfd};
use std::{
    fs,
    io::{Read as _, Write as _},
    os::fd::{AsRawFd as _, BorrowedFd},
};
use trust_resource_controller::{
    self as control,
    client::{Client, MetadataIo},
    resolver,
};

const ROLE: &str = "ATERM_RESOLVER_NATIVE_TEST_ROLE";
const STORE: &str = "ATERM_RESOLVER_NATIVE_TEST_STORE";
const CASE: &str = "ATERM_RESOLVER_NATIVE_TEST_CASE";
const PROGRESS: &str = "ATERM_RESOLVER_NATIVE_TEST_PROGRESS";
// The native profile's admitted original-scope cap; inactivity is separately bounded by HANG.
const ROOT_BUDGET: Duration = Duration::from_secs(600);
const EXACT: &str = "actor::owner::tests::resolver_native::actual_resolver_root_confines_queries_and_retains_original_owners";

#[repr(C, align(16))]
struct ProtocolMarker([u8; 64]);
#[used]
#[unsafe(link_section = "__TEXT,__trust_resolve")]
static PROTOCOL: ProtocolMarker = ProtocolMarker(resolver::RESOLVER_PROTOCOL_MARKER);

extern "C" fn entry() {
    if std::env::args_os().skip(1).collect::<Vec<_>>()
        != [OsString::from(resolver::RESOLVER_STARTUP_ARGUMENT)]
    {
        return;
    }
    let result = std::panic::catch_unwind(client);
    std::process::exit(if result.is_ok() { 0 } else { 101 });
}
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static ENTRY: extern "C" fn() = entry;

#[test]
fn actual_resolver_root_confines_queries_and_retains_original_owners() {
    run("normal");
}

#[test]
fn changed_input_after_preparation_cannot_negotiate_resolver_ready() {
    run("mutate-before-ready");
}

fn run(case: &str) {
    if std::env::var(ROLE).as_deref() == Ok("host") {
        return host();
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("resolver-host-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let store_path = scratch.path().join("host");
    let store = ImageStore::prepare_at(&store_path).unwrap();
    let program = store.cache_current().unwrap();
    let progress = scratch.path().join("progress");
    let mut command = spawnfd::command(program.path());
    command
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, "host")
        .env(CASE, case)
        .env(PROGRESS, &progress)
        .env(STORE, &store_path);
    let output = match deadline::output_while_progressing(&mut command, HANG, &mut || {
        fs::metadata(&progress).map_or(0, |metadata| metadata.len())
    }) {
        Ok(output) => output,
        Err(error) => panic!(
            "native resolver host did not complete; preserved {}: {error:?}",
            scratch.keep().display()
        ),
    };
    if !output.status.success() {
        panic!(
            "native resolver host failed; preserved {}: {output:?}",
            scratch.keep().display()
        );
    }
    assert!(String::from_utf8_lossy(&output.stdout).contains("actual-resolver-owner-retired"));
    for file in fs::read_dir(store_path.join("images")).unwrap() {
        fs::set_permissions(file.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// Advance only at real fixture phases and completed protocol exchanges, never on a poll.
fn progressed(phase: &str) {
    let path = PathBuf::from(std::env::var_os(PROGRESS).unwrap());
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(log, "{phase}").unwrap();
}

fn host() {
    progressed("host-started");
    let mutated = std::env::var(CASE).as_deref() == Ok("mutate-before-ready");
    crate::runtime::test_endpoint().unwrap();
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let store = ImageStore::open_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    progressed("image-retained");
    let socket = path.join("resolver.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&socket, 0o600).unwrap();
    let _instance = store.publish(&program, &listener).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut world = World::with_scratch(Scratch::at(&path));
    let parent = trust_resource_provenance::process::account_home()
        .unwrap()
        .join(".cache/aterm/resolver-sdk-fixtures.noindex");
    fs::create_dir_all(&parent).unwrap();
    let artifacts = aterm_tempfile::Builder::new()
        .prefix("sdk-")
        .tempdir_in(parent)
        .unwrap()
        .keep();
    eprintln!("native resolver SDK fixture: {}", artifacts.display());
    let layout = atpkg::store::Layout {
        prefix: artifacts.join("manager"),
    };
    let build = layout.build_dir("trust", 1);
    fs::create_dir_all(build.join("bin")).unwrap();
    fs::create_dir_all(build.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
    fs::create_dir_all(layout.bin_dir()).unwrap();
    let service = build.join("bin/targo");
    let compiler = build.join("bin/trustc");
    fs::copy(program.path(), &service).unwrap();
    fs::copy(program.path(), &compiler).unwrap();
    atpkg::platform::install_shim_to(&layout.bin_dir().join("targo"), &service).unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    progressed("sdk-prepared");
    let cargo_home = artifacts.join("cargo-home");
    fs::create_dir(&cargo_home).unwrap();
    let plan = aterm_json::json!({"store":path,"root":build,"program":compiler,"cwd":world.scratch.path,"mutated":mutated});
    let plan_bytes = aterm_json::to_vec(&plan).unwrap();
    let input_path = world.scratch.path.join("resolver.input");
    let mut input = File::create(&input_path).unwrap();
    input
        .write_all(&(plan_bytes.len() as u32).to_be_bytes())
        .unwrap();
    input.write_all(&plan_bytes).unwrap();
    drop(input);
    let (binding, memory, events) = world.queue_sleep(ROOT_BUDGET);
    let pending = world.owner.pending.back_mut().unwrap();
    pending.submission.request.argv = vec![
        service.as_os_str().as_bytes().to_vec(),
        resolver::RESOLVER_STARTUP_ARGUMENT.as_bytes().to_vec(),
    ];
    pending.submission.request.env = vec![
        (b"LANG".to_vec(), b"C".to_vec()),
        (b"LC_ALL".to_vec(), b"C".to_vec()),
        (
            b"CARGO_HOME".to_vec(),
            cargo_home.as_os_str().as_bytes().to_vec(),
        ),
        (b"CARGO".to_vec(), service.as_os_str().as_bytes().to_vec()),
        (b"RUSTC".to_vec(), compiler.as_os_str().as_bytes().to_vec()),
        (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
    ];
    for (key, wrong) in [
        (b"CARGO".as_slice(), b"/other/bin/targo".as_slice()),
        (b"RUSTC", b"/other/bin/trustc"),
        (b"CARGO_CACHE_RUSTC_INFO", b"1"),
        (b"CARGO_HOME", b"/source/../other"),
    ] {
        let index = pending
            .submission
            .request
            .env
            .iter()
            .position(|(name, _)| name == key)
            .unwrap();
        let original =
            std::mem::replace(&mut pending.submission.request.env[index].1, wrong.to_vec());
        assert!(
            recipe::Recipe::qualify_in(&pending.submission.request, 2, Some(&layout))
                .err()
                .unwrap()
                .to_string()
                .contains("differs from the selected SDK/profile")
        );
        pending.submission.request.env[index].1 = original;
    }
    let home = pending.submission.request.env.remove(2);
    assert!(
        recipe::Recipe::qualify_in(&pending.submission.request, 2, Some(&layout))
            .err()
            .unwrap()
            .to_string()
            .contains("explicit request-bound Cargo environment")
    );
    pending.submission.request.env.insert(2, home);
    pending
        .submission
        .request
        .env
        .push((b"RUSTC_WRAPPER".to_vec(), b"/wrapper".to_vec()));
    assert!(
        recipe::Recipe::qualify_in(&pending.submission.request, 2, Some(&layout))
            .err()
            .unwrap()
            .to_string()
            .contains("closed native profile")
    );
    pending.submission.request.env.pop();
    assert!(!world.scratch.path.join("resolver-root-ran").exists());
    pending.submission.fds.stdin = File::open(input_path).unwrap().into();
    pending.submission.fds.stdout = File::create(world.scratch.path.join("root.stdout"))
        .unwrap()
        .into();
    pending.submission.fds.stderr = File::create(world.scratch.path.join("root.stderr"))
        .unwrap()
        .into();
    pending.recipe = None;
    world.owner.recipe_layout = Some(layout.clone());
    assert_eq!(read_event(&events, &binding), Event::Queued);
    progressed("root-queued");
    let mut prepared_hold = false;
    let mut member_seen = false;
    let mut source_inputs = Vec::new();
    while !world.owner.idle() {
        world.owner.tick();
        for job in world.owner.jobs.values() {
            assert!(job.metadata_sdk.is_some());
            assert_eq!(world.owner.debt.reserved().unwrap(), 1);
            assert!(matches!(
                atpkg::lease::holders(
                    &layout.prefix,
                    &atpkg::lease::Subject::build("trust", 1).unwrap()
                ),
                atpkg::lease::Holders::Held(_)
            ));
            if !job.launched && job.resolver.is_some() {
                prepared_hold = true;
                assert!(
                    !world.scratch.path.join("resolver-root-ran").exists(),
                    "selected resolver cannot run before its two native holds"
                );
            }
            if !job.members.is_empty() {
                member_seen = true;
                assert!(job.resolver_ready);
                assert!((1..=resolver::RESOLVER_MAX_QUERIES).contains(&job.resolver_queries));
            }
        }
        match aterm_uds::kept_out(|| listener.accept()) {
            Ok((stream, _)) => {
                let end = Instant::now() + HANG;
                let peer = match wire::local_peer(&stream) {
                    Ok(peer) => peer,
                    // Discovery can close without sending a request. Darwin
                    // then drops peer credentials; no execution is authorized.
                    Err(error) if error.kind() == io::ErrorKind::NotConnected => continue,
                    Err(error) => panic!("resolver native peer: {error}"),
                };
                let hello = match crate::runtime::read_json(&stream, end) {
                    Ok(hello) => hello,
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => continue,
                    Err(error) => panic!("resolver hello: {error}"),
                };
                let role = hello["role"].as_str().unwrap();
                match role {
                    "native-resolver-ready-v1" => {
                        if mutated {
                            assert!(world.scratch.path.join("resolver-root-ran").is_file());
                            let mode = fs::metadata(&service).unwrap().permissions().mode();
                            fs::set_permissions(&service, fs::Permissions::from_mode(mode ^ 0o200))
                                .unwrap();
                        }
                        crate::resource_context::serve_resolver_ready(
                            &stream,
                            peer,
                            &hello,
                            end,
                            |peer, query| world.owner.resolver_ready(peer, &query),
                        )
                        .unwrap();
                    }
                    "sealed-resolver-query-v1" => {
                        let selected =
                            members::binding_role(&hello, resolver::RESOLVER_QUERY_CAPABILITY)
                                .unwrap();
                        wire::write_frame(&stream, aterm_types::resource_subgroup::PREPARE, end)
                            .unwrap();
                        let incoming = crate::resolver_transport::receive_authorized(
                            &stream,
                            &selected,
                            end,
                            |request, peer| {
                                let mut check = members::Authorization::of(request, peer).unwrap();
                                check.resolver = true;
                                world.owner.authorize_member(&check)
                            },
                        );
                        match incoming {
                            Ok(mut incoming) => {
                                // Exercise the actual GO boundary with an owned
                                // nonempty source descriptor. The typed client
                                // ordinarily sends /dev/null; a direct peer need not.
                                let (reader, mut writer) = io::pipe().unwrap();
                                writer
                                    .write_all(b"caller source must never be consumed\n")
                                    .unwrap();
                                drop(writer);
                                source_inputs.push(reader.try_clone().unwrap());
                                incoming.subgroup.rights.stdin = reader.into();
                                world.owner.submit_resolver_query(incoming);
                                aterm_ctl::resource_subgroup_client::acknowledge(&stream, end)
                                    .unwrap();
                            }
                            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
                            Err(error) => panic!("resolver attachment: {error}"),
                        }
                    }
                    role => panic!("unexpected resolver role {role}"),
                }
                progressed(role);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("resolver listener: {error}"),
        }
        if !world.owner.idle() {
            std::thread::sleep(POLL);
        }
    }
    let Event::Started { pid, .. } = read_event(&events, &binding) else {
        panic!("resolver never Started");
    };
    let Event::Finished { outcome, cause, .. } = read_event(&events, &binding) else {
        panic!("resolver never Finished");
    };
    assert_eq!(
        outcome,
        Outcome::Exit(0),
        "{}",
        fs::read_to_string(world.scratch.path.join("root.stderr")).unwrap()
    );
    assert_eq!(
        cause,
        if mutated {
            // The real root exits successfully after the expected refusal;
            // final SDK integrity still independently fails the original job.
            aterm_types::resource_admission::CompletionCause::HostFault
        } else {
            aterm_types::resource_admission::CompletionCause::Exited
        }
    );
    assert!(prepared_hold);
    assert_eq!(member_seen, !mutated);
    if mutated {
        assert!(!world.scratch.path.join("metadata-target-started").exists());
    }
    for mut original in source_inputs {
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"caller source must never be consumed\n");
    }
    assert!(
        world
            .scratch
            .path
            .join("resolver-client-complete")
            .is_file()
    );
    world.assert_settled(&binding, &memory, pid);
    assert!(matches!(
        atpkg::lease::holders(
            &layout.prefix,
            &atpkg::lease::Subject::build("trust", 1).unwrap()
        ),
        atpkg::lease::Holders::Free
    ));
    println!("actual-resolver-owner-retired cpu=0 memory=closed SDK=free");
    fs::remove_file(socket).unwrap();
    fs::remove_dir_all(artifacts).unwrap();
}

fn client() {
    // The selected root starts under a positively installed OS no-fork policy,
    // before even this fixture's first semantic input or host negotiation.
    assert!(crate::metadata_prelude::no_fork(std::process::id()).unwrap());
    let fork = unsafe { libc::fork() };
    if fork == 0 {
        unsafe { libc::_exit(93) }
    }
    assert_eq!(fork, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
    let keeper = control::client::sdk::snapshot_sdk_lease().unwrap();
    let descriptions = unsafe {
        [
            BorrowedFd::borrow_raw(3).try_clone_to_owned().unwrap(),
            BorrowedFd::borrow_raw(4).try_clone_to_owned().unwrap(),
        ]
    };
    unsafe {
        control::client::sdk::capture_sdk_lease(keeper).unwrap();
    }
    let mut length = [0; 4];
    std::io::stdin().read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length < 65536);
    let mut bytes = vec![0; length];
    std::io::stdin().read_exact(&mut bytes).unwrap();
    let plan: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    let cwd = std::path::Path::new(plan["cwd"].as_str().unwrap());
    fs::write(cwd.join("resolver-root-ran"), "actual selected root").unwrap();
    let deadline = || Instant::now() + HANG;
    let caller = Client::discover_at(
        std::path::Path::new(plan["store"].as_str().unwrap()),
        deadline(),
    )
    .unwrap();
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let program = PathBuf::from(plan["program"].as_str().unwrap());
    let request = resolver::SealedResolverQueryRequest {
        member: control::Request {
            binding: binding.clone(),
            member_attempt: [84; 16],
            role: control::Role::ResolverQuery,
            command: control::CommandSpec {
                program: program.as_os_str().as_bytes().to_vec(),
                arg0: None,
                args: vec![b"-vV".to_vec()],
                environment: [
                    "LANG",
                    "LC_ALL",
                    "CARGO",
                    "RUSTC",
                    "CARGO_HOME",
                    "CARGO_CACHE_RUSTC_INFO",
                ]
                .into_iter()
                .map(|name| {
                    (
                        name.as_bytes().to_vec(),
                        std::env::var_os(name).unwrap().as_bytes().to_vec(),
                    )
                })
                .collect(),
                cwd: cwd.as_os_str().as_bytes().to_vec(),
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
            timeout_ns: Some(resolver::RESOLVER_MAX_QUERY_NS),
        },
        root: plan["root"].as_str().unwrap().as_bytes().to_vec(),
        program_sha256: aterm_digest::Sha256::digest(fs::read(&program).unwrap()),
    };
    let duplicated = || {
        let copies: Vec<_> = descriptions
            .iter()
            .map(|fd| fd.try_clone().unwrap())
            .collect();
        let mut copied = request.clone();
        copied.member.delegations.gnu = control::GnuTransport::NumericPipe {
            read: control::Descriptor {
                source: copies[0].as_raw_fd() as u32,
                target: 3,
            },
            write: control::Descriptor {
                source: copies[1].as_raw_fd() as u32,
                target: 4,
            },
        };
        (
            copied,
            MetadataIo {
                descriptions: copies,
            },
        )
    };
    // A valid native root and inherited SDK are insufficient before the
    // separately negotiated resolver-ready transition.
    let (unready, unready_io) = duplicated();
    assert!(matches!(
        caller.resolver_query(&unready, unready_io, deadline()),
        Err(control::client::MetadataError::Refused)
    ));
    let query = control::ContextQuery {
        binding,
        call_attempt: [85; 16],
    };
    if plan["mutated"].as_bool() == Some(true) {
        assert!(
            caller.resolver_context(&query, deadline()).is_err(),
            "changed original input after preparation must refuse readiness"
        );
        fs::write(
            cwd.join("resolver-client-complete"),
            "actual input-change refusal",
        )
        .unwrap();
        return;
    }
    let ready = caller.resolver_context(&query, deadline()).unwrap();
    assert_eq!(ready.context.original_root.pid, std::process::id());
    assert_eq!(ready.context.sdk.as_ref().unwrap().execution, request.root);
    control::client::sdk::validate_sdk_keeper(&ready.context).unwrap();
    assert!(
        caller
            .resolver_context(
                &control::ContextQuery {
                    call_attempt: [86; 16],
                    ..query
                },
                deadline()
            )
            .is_err()
    );
    let (live, live_io) = duplicated();
    let execute = |request: &resolver::SealedResolverQueryRequest, io: MetadataIo| {
        let until = deadline();
        let mut member = caller.resolver_query(request, io, until).unwrap();
        loop {
            if let Some(result) = member.poll(until).unwrap() {
                break result;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let result = execute(&live, live_io);
    assert!(result.completion.inputs_unchanged.is_ok());
    assert_eq!(result.prepared.root, request.root);
    assert!(matches!(
        result.completion.finished.status,
        control::Status::Finished {
            outcome: control::Outcome::Exit(0),
            cause: control::CompletionCause::Exited,
            ..
        }
    ));
    assert_eq!(result.stdout, b"actual native metadata output\n");
    let (replay, replay_io) = duplicated();
    assert!(
        matches!(
            caller.resolver_query(&replay, replay_io, deadline()),
            Err(control::client::MetadataError::Refused)
        ),
        "completed attempt must never replay"
    );
    let (mut foreign, foreign_io) = duplicated();
    foreign.member.member_attempt = [87; 16];
    foreign.root.extend_from_slice(b"-foreign");
    foreign.member.command.program = [foreign.root.as_slice(), b"/bin/trustc"].concat();
    for (name, value) in &mut foreign.member.command.environment {
        if name == b"RUSTC" {
            *value = foreign.member.command.program.clone();
        }
        if name == b"CARGO" {
            *value = [foreign.root.as_slice(), b"/bin/targo"].concat();
        }
    }
    assert!(
        matches!(
            caller.resolver_query(&foreign, foreign_io, deadline()),
            Err(control::client::MetadataError::Refused)
        ),
        "another SDK's compiler cannot borrow this resolver root"
    );
    let (mut foreign_env, foreign_env_io) = duplicated();
    foreign_env.member.member_attempt = [89; 16];
    for (name, value) in &mut foreign_env.member.command.environment {
        if name == b"CARGO_HOME" {
            *value = b"/another-normalized-cargo-home".to_vec();
        }
    }
    assert!(
        matches!(
            caller.resolver_query(&foreign_env, foreign_env_io, deadline()),
            Err(control::client::MetadataError::Refused)
        ),
        "query environment must match the original root"
    );
    for number in 1..resolver::RESOLVER_MAX_QUERIES {
        let (mut next, next_io) = duplicated();
        next.member.member_attempt = [100 + number as u8; 16];
        let result = execute(&next, next_io);
        assert!(result.completion.inputs_unchanged.is_ok());
        assert!(matches!(
            result.completion.finished.status,
            control::Status::Finished {
                outcome: control::Outcome::Exit(0),
                cause: control::CompletionCause::Exited,
                ..
            }
        ));
        assert_eq!(result.stdout, b"actual native metadata output\n");
    }
    let (mut excess, excess_io) = duplicated();
    excess.member.member_attempt = [120; 16];
    assert!(
        matches!(
            caller.resolver_query(&excess, excess_io, deadline()),
            Err(control::client::MetadataError::Refused)
        ),
        "seventeenth request must refuse before admission"
    );
    fs::write(
        cwd.join("resolver-client-complete"),
        "actual completion and EOF",
    )
    .unwrap();
}
