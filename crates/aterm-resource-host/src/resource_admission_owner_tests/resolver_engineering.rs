// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit engineering interoperability, not distribution/source qualification.
//! The whole private SDK is copied from a held managed build; only its Targo is
//! replaced with the independently selected resolver-capable engineering image.
use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_uds::{CtlListener, deadline, spawnfd};
use std::{ffi::CString, fs, os::unix::fs::MetadataExt as _, path::Path};
use trust_resource_controller::{self as control, resolver};

const ROLE: &str = "ATERM_RESOLVER_ENGINEERING_ROLE";
const STORE: &str = "ATERM_RESOLVER_ENGINEERING_STORE";
const IMAGE: &str = "ATERM_RESOLVER_ENGINEERING_IMAGE";
const IMAGE_SHA: &str = "ATERM_RESOLVER_ENGINEERING_SHA256";
const SDK: &str = "ATERM_RESOLVER_ENGINEERING_SOURCE_SDK";
const EXACT: &str = "actor::owner::tests::resolver_engineering::actual_targo_resolves_locked_path_dependency_under_original_native_owners";
const ROOT_LIMIT: Duration = Duration::from_secs(600);

#[test]
#[ignore = "requires an explicitly pinned resolver-capable Targo and a managed source SDK"]
fn actual_targo_resolves_locked_path_dependency_under_original_native_owners() {
    if std::env::var(ROLE).as_deref() == Ok("host") {
        return host();
    }
    // Explicit inputs are checked before creating or admitting any native job.
    for name in [IMAGE, IMAGE_SHA, SDK] {
        assert!(std::env::var_os(name).is_some(), "missing explicit {name}");
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("resolver-engineering-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap()
        .keep();
    // A failed or unknown child never authorizes deletion of its cached image,
    // original journal or private capacity state.
    let store_path = scratch.join("host");
    let store = ImageStore::prepare_at(&store_path).unwrap();
    let program = store.cache_current().unwrap();
    let mut command = spawnfd::command(program.path());
    command
        .args(["--exact", EXACT, "--ignored", "--nocapture"])
        .env(ROLE, "host")
        .env(STORE, &store_path);
    let mut observed = 0;
    let output = deadline::output_while_progressing_until(
        &mut command,
        HANG,
        Instant::now() + ROOT_LIMIT + HANG * 2,
        &mut || {
            if let Ok(bytes) = fs::read(store_path.join("progress"))
                && let Ok(bytes) = <[u8; 8]>::try_from(bytes.as_slice())
            {
                observed = observed.max(u64::from_be_bytes(bytes));
            }
            observed
        },
    )
    .unwrap_or_else(|error| {
        panic!(
            "real resolver host wait failed ({error}); preserved {}",
            scratch.display()
        )
    });
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        panic!(
            "real Targo resolver failed; preserved {}: {output:?}",
            scratch.display()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    assert!(stdout.contains("actual-targo-resolver-retired packages=2 nodes=2 members=1"));
    for file in fs::read_dir(store_path.join("images")).unwrap() {
        fs::set_permissions(file.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::remove_dir_all(scratch).unwrap();
}

fn progress(path: &Path, sequence: &mut u64, stage: &str) {
    *sequence += 1;
    fs::write(path.join("progress.stage"), stage).unwrap();
    fs::write(path.join("progress"), sequence.to_be_bytes()).unwrap();
}

/// Native COW copy, with independent inodes and no source-tree writes/hardlinks.
/// The original atpkg in-use lease remains held for this entire operation.
fn clone_sdk(source: &Path, destination: &Path) {
    unsafe extern "C" {
        fn clonefile(
            source: *const libc::c_char,
            destination: *const libc::c_char,
            flags: u32,
        ) -> i32;
    }
    let source_c = CString::new(source.as_os_str().as_bytes()).unwrap();
    let destination_c = CString::new(destination.as_os_str().as_bytes()).unwrap();
    // CLONE_NOFOLLOW | CLONE_NOOWNERCOPY; a directory clone preserves its full
    // contents and symlinks rather than following links into another tree.
    let result = unsafe { clonefile(source_c.as_ptr(), destination_c.as_ptr(), 3) };
    assert_eq!(
        result,
        0,
        "complete private SDK clone: {}",
        io::Error::last_os_error()
    );
    for relative in ["bin/trustc", "bin/targo"] {
        let original = fs::metadata(source.join(relative)).unwrap();
        let copied = fs::metadata(destination.join(relative)).unwrap();
        assert_ne!(
            (original.dev(), original.ino()),
            (copied.dev(), copied.ino())
        );
        assert_eq!(original.len(), copied.len());
    }
}

fn workspace(root: &Path) -> Vec<u8> {
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::create_dir_all(root.join("dep/src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=[\"app\"]\nexclude=[\"dep\"]\nresolver=\"2\"\n",
    )
    .unwrap();
    fs::write(root.join("app/Cargo.toml"), "[package]\nname=\"resolver-engineering-app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nresolver-engineering-dep={path=\"../dep\"}\n").unwrap();
    fs::write(
        root.join("dep/Cargo.toml"),
        "[package]\nname=\"resolver-engineering-dep\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    fs::write(
        root.join("app/src/lib.rs"),
        "pub fn value()->u32 { resolver_engineering_dep::value() }\n",
    )
    .unwrap();
    fs::write(root.join("dep/src/lib.rs"), "pub fn value()->u32 { 42 }\n").unwrap();
    let lock = b"version = 4\n\n[[package]]\nname = \"resolver-engineering-app\"\nversion = \"0.1.0\"\ndependencies = [\n \"resolver-engineering-dep\",\n]\n\n[[package]]\nname = \"resolver-engineering-dep\"\nversion = \"0.1.0\"\n".to_vec();
    fs::write(root.join("Cargo.lock"), &lock).unwrap();
    lock
}

fn save(root: &Path, name: &str, bytes: &[u8]) {
    fs::write(root.join(name), bytes).unwrap();
    fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o400)).unwrap();
}

fn lease_held(layout: &atpkg::store::Layout) {
    assert!(matches!(
        atpkg::lease::holders(
            &layout.prefix,
            &atpkg::lease::Subject::build("trust", 1).unwrap()
        ),
        atpkg::lease::Holders::Held(_)
    ));
}

fn host() {
    crate::runtime::test_endpoint().unwrap();
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let mut sequence = 0;
    progress(&path, &mut sequence, "native host started");
    let store = ImageStore::open_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    let socket = path.join("resolver.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&socket, 0o600).unwrap();
    let _instance = store.publish(&program, &listener).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut world = World::with_scratch(Scratch::at(&path));
    let parent = trust_resource_provenance::process::account_home()
        .unwrap()
        .join(".cache/aterm/.ubd/resolver-engineering.noindex");
    fs::create_dir_all(&parent).unwrap();
    let artifacts = aterm_tempfile::Builder::new()
        .prefix("actual-")
        .tempdir_in(parent)
        .unwrap()
        .keep();
    eprintln!("real resolver inputs/evidence: {}", artifacts.display());
    let source = fs::canonicalize(PathBuf::from(std::env::var_os(SDK).unwrap())).unwrap();
    let source_layout = atpkg::store::resolve_configured().unwrap();
    assert!(atpkg::lease::Subject::of_dir(&source_layout.prefix, &source.join("bin")).is_some());
    let source_lease = owner_fixture_sdk_lease(
        &source_layout.prefix,
        &source.join("bin"),
        "complete private resolver engineering SDK copy",
        Instant::now(),
    )
    .unwrap();
    let layout = atpkg::store::Layout {
        prefix: artifacts.join("manager"),
    };
    let build = layout.build_dir("trust", 1);
    fs::create_dir_all(build.parent().unwrap()).unwrap();
    clone_sdk(&source, &build);
    drop(source_lease);
    progress(
        &path,
        &mut sequence,
        "complete independent SDK clone finished",
    );
    let service = build.join("bin/targo");
    let compiler = build.join("bin/trustc");
    let image = PathBuf::from(std::env::var_os(IMAGE).unwrap());
    let mut image_sha = [0; 32];
    assert!(aterm_codec::hex::decode_into(
        std::env::var(IMAGE_SHA).unwrap().as_bytes(),
        &mut image_sha
    ));
    assert_eq!(
        aterm_digest::Sha256::digest(fs::read(&image).unwrap()),
        image_sha
    );
    // This clone path has never executed. Replacement is not an installed SDK
    // mutation and makes no coherent distribution/source-authority claim.
    fs::remove_file(&service).unwrap();
    fs::copy(&image, &service).unwrap();
    fs::create_dir_all(layout.bin_dir()).unwrap();
    atpkg::platform::install_shim_to(&layout.bin_dir().join("targo"), &service).unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    progress(
        &path,
        &mut sequence,
        "exact engineering image installed in private manager",
    );
    let cargo_home = artifacts.join("cargo-home");
    fs::create_dir(&cargo_home).unwrap();
    let cwd = artifacts.join("workspace");
    let original_lock = workspace(&cwd);
    let invocation = resolver::ResolverInvocation {
        host_store: Some(path.as_os_str().as_bytes().to_vec()),
        root: build.as_os_str().as_bytes().to_vec(),
        program: service.as_os_str().as_bytes().to_vec(),
        program_sha256: image_sha,
        compiler: compiler.as_os_str().as_bytes().to_vec(),
        compiler_sha256: aterm_digest::Sha256::digest(fs::read(&compiler).unwrap()),
        cwd: cwd.as_os_str().as_bytes().to_vec(),
        cargo_home: cargo_home.as_os_str().as_bytes().to_vec(),
    };
    save(
        &artifacts,
        "invocation.frame",
        &invocation.encode().unwrap(),
    );
    save(&artifacts, "original.lock", &original_lock);
    let (binding, memory, events) = world.queue_sleep(ROOT_LIMIT);
    let queued = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    assert_eq!(
        EventFrame::decode(&queued, &binding).unwrap().event,
        Event::Queued
    );
    save(&artifacts, "root-queued.frame", &queued);
    let pending = world.owner.pending.back_mut().unwrap();
    pending.submission.request.argv = vec![
        invocation.program.clone(),
        resolver::RESOLVER_STARTUP_ARGUMENT.as_bytes().to_vec(),
    ];
    pending.submission.request.cwd = invocation.cwd.clone();
    pending.submission.request.env = vec![
        (b"LANG".to_vec(), b"C".to_vec()),
        (b"LC_ALL".to_vec(), b"C".to_vec()),
        (b"CARGO_HOME".to_vec(), invocation.cargo_home.clone()),
        (b"CARGO".to_vec(), invocation.program.clone()),
        (b"RUSTC".to_vec(), invocation.compiler.clone()),
        (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
    ];
    pending.submission.fds.stdin = File::open(artifacts.join("invocation.frame"))
        .unwrap()
        .into();
    pending.submission.fds.stdout = File::create(artifacts.join("root.stdout")).unwrap().into();
    pending.submission.fds.stderr = File::create(artifacts.join("root.stderr")).unwrap().into();
    pending.recipe = None;
    world.owner.recipe_layout = Some(layout.clone());
    progress(&path, &mut sequence, "original native root queued");
    let until = Instant::now() + ROOT_LIMIT;
    let mut prepared_hold = false;
    let mut root_started = false;
    let mut members = 0;
    while !world.owner.idle() {
        world.owner.tick();
        for job in world.owner.jobs.values() {
            assert!(job.metadata_sdk.is_some());
            assert_eq!(world.owner.debt.reserved().unwrap(), 1);
            lease_held(&layout);
            if !job.launched && job.resolver.is_some() && !prepared_hold {
                prepared_hold = true;
                progress(
                    &path,
                    &mut sequence,
                    "original CPU/memory adopted; selected root preparing",
                );
            }
            if job.launched && !root_started {
                root_started = true;
                progress(
                    &path,
                    &mut sequence,
                    "selected native root started after both holds",
                );
            }
            if !job.members.is_empty() {
                assert!(job.resolver_ready);
            }
        }
        match aterm_uds::kept_out(|| listener.accept()) {
            Ok((stream, _)) => {
                let end = crate::fixture_deadline(Instant::now());
                let peer = match wire::local_peer(&stream) {
                    Ok(peer) => peer,
                    Err(error) if error.kind() == io::ErrorKind::NotConnected => continue,
                    Err(error) => panic!("real resolver peer: {error}"),
                };
                let hello = match crate::runtime::read_json(&stream, end) {
                    Ok(hello) => hello,
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => continue,
                    Err(error) => panic!("real resolver hello: {error}"),
                };
                match hello["role"].as_str().unwrap() {
                    resolver::RESOLVER_READY_ROLE => {
                        assert!(crate::metadata_prelude::no_fork(peer.pid).unwrap());
                        crate::resource_context::serve_resolver_ready(
                            &stream,
                            peer,
                            &hello,
                            end,
                            |peer, query| world.owner.resolver_ready(peer, &query),
                        )
                        .unwrap();
                        progress(
                            &path,
                            &mut sequence,
                            "same registered root negotiated resolver readiness",
                        );
                    }
                    resolver::RESOLVER_QUERY_CAPABILITY => {
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
                        )
                        .unwrap();
                        members += 1;
                        save(
                            &artifacts,
                            &format!("query-{members}.decoded-request.frame"),
                            &incoming.sealed.encode().unwrap(),
                        );
                        world.owner.submit_resolver_query(incoming);
                        aterm_ctl::resource_subgroup_client::acknowledge(&stream, end).unwrap();
                        progress(
                            &path,
                            &mut sequence,
                            "original paid compiler query admitted",
                        );
                    }
                    role => panic!("unexpected real resolver role {role}"),
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("real resolver listener: {error}"),
        }
        assert!(
            Instant::now() < until,
            "real resolver did not drain original owners"
        );
        if !world.owner.idle() {
            std::thread::sleep(POLL);
        }
    }
    progress(
        &path,
        &mut sequence,
        "original native root and all members drained",
    );
    let started = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    let Event::Started { pid, .. } = EventFrame::decode(&started, &binding).unwrap().event else {
        panic!("real resolver not Started")
    };
    save(&artifacts, "root-started.frame", &started);
    let finished = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    save(&artifacts, "root-finished.frame", &finished);
    let Event::Finished { outcome, cause, .. } =
        EventFrame::decode(&finished, &binding).unwrap().event
    else {
        panic!("real resolver not Finished")
    };
    let stdout = fs::read(artifacts.join("root.stdout")).unwrap();
    let stderr = fs::read(artifacts.join("root.stderr")).unwrap();
    eprintln!(
        "actual resolver stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    fs::set_permissions(
        artifacts.join("root.stdout"),
        fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    fs::set_permissions(
        artifacts.join("root.stderr"),
        fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    // Failed semantic resolution still must actually drain its original paid
    // scope. Check this independently, before judging the report's success.
    world.assert_settled(&binding, &memory, pid);
    assert!(matches!(
        atpkg::lease::holders(
            &layout.prefix,
            &atpkg::lease::Subject::build("trust", 1).unwrap()
        ),
        atpkg::lease::Holders::Free
    ));
    eprintln!("actual resolver root {outcome:?}/{cause:?}; original CPU=0 memory=closed SDK=free");
    // Parse the actual binary report even for a failed root; never reinterpret
    // kind34 entry stdout as normal metadata JSON or manufacture a completion.
    let report = resolver::ResolverReport::decode(&stdout, &invocation).unwrap();
    assert!(
        report.failure.is_none(),
        "resolver report failure: {:?}",
        report.failure
    );
    assert_eq!(outcome, Outcome::Exit(0));
    assert_eq!(
        cause,
        aterm_types::resource_admission::CompletionCause::Exited
    );
    assert!(prepared_hold && members > 0);
    assert_eq!(members, report.queries.len());
    let runner = report.runner.as_ref().unwrap();
    assert_eq!(runner.identity.pid, pid);
    let context = control::ContextReply::decode_observation(&runner.context_frame)
        .unwrap()
        .context;
    assert_eq!(context.original_root.pid, pid);
    assert_eq!(context.root_attempt, binding.attempt);
    assert_eq!(context.sdk.as_ref().unwrap().execution, invocation.root);
    for query in &report.queries {
        assert!(query.failure.is_none());
        let result = query
            .result
            .as_ref()
            .expect("every actual compiler query completed");
        assert!(!result.output_limit_exceeded);
        assert!(result.completion.inputs_unchanged.is_ok());
        assert_eq!(
            result.prepared.executable.sha256,
            invocation.compiler_sha256
        );
        assert_ne!(result.started.identity.pid, pid);
        assert!(matches!(
            result.completion.finished.status,
            control::Status::Finished {
                outcome: control::Outcome::Exit(0),
                cause: control::CompletionCause::Exited,
                ..
            }
        ));
    }
    let metadata = report.metadata.as_ref().unwrap();
    save(&artifacts, "metadata.json", metadata);
    assert_full_dependency_population(metadata, &cwd);
    assert_eq!(fs::read(cwd.join("Cargo.lock")).unwrap(), original_lock);
    fs::remove_file(socket).unwrap();
    drop(world);
    assert_eq!(fs::read(artifacts.join("root.stdout")).unwrap(), stdout);
    assert_eq!(
        fs::read(artifacts.join("invocation.frame")).unwrap(),
        invocation.encode().unwrap()
    );
    println!(
        "actual resolver invocation sha256={} bytes={} report sha256={} bytes={}",
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(invocation.encode().unwrap())),
        invocation.encode().unwrap().len(),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&stdout)),
        stdout.len()
    );
    println!(
        "actual-targo-resolver-retired packages=2 nodes=2 members=1 queries={members} cpu=0 memory=closed SDK=free evidence={}",
        artifacts.display()
    );
}

fn assert_full_dependency_population(bytes: &[u8], cwd: &Path) {
    let metadata: aterm_json::Value = aterm_json::from_slice(bytes).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    assert_eq!(
        packages.len(),
        2,
        "full path dependency population is mandatory"
    );
    let package = |name: &str| {
        packages
            .iter()
            .find(|p| p["name"].as_str() == Some(name))
            .unwrap()
    };
    let app = package("resolver-engineering-app");
    let dep = package("resolver-engineering-dep");
    assert_eq!(
        app["manifest_path"].as_str(),
        cwd.join("app/Cargo.toml").to_str()
    );
    assert_eq!(
        dep["manifest_path"].as_str(),
        cwd.join("dep/Cargo.toml").to_str()
    );
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("full resolve graph, never --no-deps");
    assert_eq!(nodes.len(), 2);
    let app_node = nodes.iter().find(|node| node["id"] == app["id"]).unwrap();
    let dep_node = nodes.iter().find(|node| node["id"] == dep["id"]).unwrap();
    assert_eq!(
        app_node["dependencies"].as_array().unwrap(),
        &[dep["id"].clone()]
    );
    assert!(dep_node["dependencies"].as_array().unwrap().is_empty());
    assert_eq!(
        metadata["workspace_members"].as_array().unwrap(),
        &[app["id"].clone()]
    );
    assert_eq!(
        metadata["workspace_default_members"].as_array().unwrap(),
        &[app["id"].clone()]
    );
    assert_eq!(metadata["workspace_root"].as_str(), cwd.to_str());
}
