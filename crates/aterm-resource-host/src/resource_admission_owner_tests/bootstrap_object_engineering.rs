// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real selected engineering SDK, actual Root, production actor/RPC/native issuer.
//! World controls private admission headroom; this is not installed GUI or machine-pressure
//! qualification. It never constructs Ready, copies a marker, or manufactures an SDK owner.

use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_types::resource_admission::{AccountedEventFrame, CompletionCause};
use aterm_uds::{CtlListener, deadline, spawnfd};
use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};
use trust_resource_controller::bootstrap_graph::object;

const EXACT: &str = "actor::owner::tests::bootstrap_object_engineering::actual_selected_sdk_native_object_uses_original_root_and_owners";
const ROLE: &str = "ATERM_BGRAPH_OBJECT_FIXTURE_ROLE";
const STORE: &str = "ATERM_BGRAPH_OBJECT_FIXTURE_STORE";
const PREFIX: &str = "ATERM_BGRAPH_OBJECT_SDK_PREFIX";
const BUILD: &str = "ATERM_BGRAPH_OBJECT_SDK_BUILD";
const IMAGE_SHA: &str = "ATERM_BGRAPH_OBJECT_TARGO_SHA256";
const SOURCE: &str = "ATERM_BGRAPH_OBJECT_SOURCE_ROOT";
const SHIMS: &str = "ATERM_BGRAPH_OBJECT_SHIMS";
const ROOT_LIMIT: Duration = Duration::from_secs(600);
const SUCCESS: &str = "actual-bootstrap-object-retired";

fn required(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing explicit {name}")))
}
fn canonical(path: &Path) -> PathBuf {
    assert!(
        path.is_absolute(),
        "absolute qualification input required: {}",
        path.display()
    );
    assert_eq!(
        fs::canonicalize(path).unwrap(),
        path,
        "input must already be physical/canonical"
    );
    path.to_owned()
}

#[test]
#[ignore = "requires a genuinely produced private engineering SDK, exact final Targo SHA, isolated original SourceRoot and qualified Bootstrap shims"]
fn actual_selected_sdk_native_object_uses_original_root_and_owners() {
    if std::env::var(ROLE).as_deref() == Ok("host") {
        return host();
    }
    // All explicit inputs are required before caching, queueing, or any resource owner.
    for name in [PREFIX, BUILD, IMAGE_SHA, SOURCE, SHIMS] {
        assert!(std::env::var_os(name).is_some(), "missing explicit {name}");
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("bgraph-object-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap()
        .keep();
    let store_path = scratch.join("host");
    let store = ImageStore::prepare_at(&store_path).unwrap();
    let program = store.cache_current().unwrap();
    let mut command = spawnfd::command(program.path());
    command
        .args(["--exact", EXACT, "--ignored", "--nocapture"])
        .env(ROLE, "host")
        .env(STORE, &store_path);
    let result =
        deadline::output_within(&mut command, ROOT_LIMIT + HANG * 2).unwrap_or_else(|why| {
            panic!(
                "actual Host fixture did not finish ({why}); retained {}",
                scratch.display()
            )
        });
    fs::write(scratch.join("host.stdout"), &result.stdout).unwrap();
    fs::write(scratch.join("host.stderr"), &result.stderr).unwrap();
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    println!("{}", String::from_utf8_lossy(&result.stdout));
    assert!(
        result.status.success(),
        "actual fixture failed; retained {}",
        scratch.display()
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains(SUCCESS),
        "exact body/postconditions did not execute"
    );
    // Never delete the SourceRoot, SDK, journals or evidence: OPEN/WITHHELD is intentional.
    println!("real-native-first-node evidence={}", scratch.display());
}

struct Actor(Option<std::thread::JoinHandle<()>>);
impl Actor {
    fn finish(&mut self) {
        assert!(crate::actor::stop_test().unwrap());
        self.0.take().unwrap().join().unwrap();
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        if self.0.is_none() {
            return;
        }
        let until = Instant::now() + HANG;
        // Same actual owner cancellation, never PID guessing or status-derived release.
        while Instant::now() < until {
            if crate::actor::stop_test().unwrap_or(false) {
                if let Some(thread) = self.0.take() {
                    let _ = thread.join();
                }
                break;
            }
            std::thread::sleep(POLL);
        }
    }
}
fn host() {
    let path = canonical(&required(STORE));
    // Real original discovery directory must exist BEFORE Owner/prelude preparation.
    crate::runtime::test_endpoint_with_store(&path).unwrap();
    let store = ImageStore::open_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    let listener = CtlListener::bind(path.join("host.sock")).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&path.join("host.sock"), 0o600).unwrap();
    let _published = store.publish(&program, &listener).unwrap();
    listener.set_nonblocking(true).unwrap();
    let layout = atpkg::store::Layout {
        prefix: canonical(&required(PREFIX)),
    };
    let build: u64 = std::env::var(BUILD).unwrap().parse().unwrap();
    let sdk = atpkg::lookup::payload_root(&layout, "trust", build)
        .expect("actual managed physical engineering payload");
    let sdk = canonical(&sdk);
    let source = canonical(&required(SOURCE));
    let shims = canonical(&required(SHIMS));
    assert!(!source.starts_with(&sdk) && !sdk.starts_with(&source));
    assert!(
        source.join(".git").exists(),
        "real unchanged qualification source must be Git-backed"
    );
    assert_eq!(
        fs::read(source.join("src/bootstrap/native_object.rs")).unwrap(),
        object::SOURCE_BYTES
    );
    let image = sdk.join("bin/targo");
    let compiler = sdk.join("bin/trustc");
    let mut expected = [0; 32];
    assert!(aterm_codec::hex::decode_into(
        std::env::var(IMAGE_SHA).unwrap().as_bytes(),
        &mut expected
    ));
    assert_eq!(
        aterm_digest::Sha256::digest(fs::read(&image).unwrap()),
        expected
    );
    let subject = atpkg::lease::Subject::build("trust", build).unwrap();
    assert!(
        matches!(
            atpkg::lease::holders(&layout.prefix, &subject),
            atpkg::lease::Holders::Free
        ),
        "qualification begins without a preexisting private SDK user"
    );
    let artifacts = path.join("first-node");
    fs::create_dir(&artifacts).unwrap();
    let output = artifacts.join("output");
    fs::create_dir(&output).unwrap();
    let cargo_home = artifacts.join("cargo-home");
    fs::create_dir(&cargo_home).unwrap();
    let config = artifacts.join("bootstrap.toml");
    let quoted = |value: &Path| aterm_json::to_string(value.to_str().unwrap()).unwrap();
    fs::write(&config,format!("[build]\nbuild = \"aarch64-apple-darwin\"\nhost = [\"aarch64-apple-darwin\"]\ntarget = [\"aarch64-apple-darwin\"]\nrustc = {}\ncargo = {}\nsubmodules = false\n[rust]\nchannel = \"auto-detect\"\nomit-git-hash = false\ndownload-rustc = false\n[llvm]\ndownload-ci-llvm = false\n",quoted(&compiler),quoted(&image))).unwrap();
    let stdout = artifacts.join("root.stdout");
    let stderr = artifacts.join("root.stderr");
    let mut scratch = Scratch::at(&path);
    scratch.preserve = true;
    // Move the same original Owner into the actor, keeping its native namespace
    // and retirement alive. World has Drop and cannot transfer its Owner.
    let (mut owner, _namespace, _retirement) = World::prepare(&scratch);
    let (binding, memory, events) = queue_owned(&mut owner, &scratch.path, ROOT_LIMIT, [7; 16]);
    let pending = owner.pending.back_mut().unwrap();
    pending.events.account().unwrap();
    let original_cwd = source.as_os_str().as_bytes().to_vec();
    let args = vec![
        image.into_os_string(),
        "--unverified".into(),
        "resource".into(),
        "bootstrap-runtime".into(),
        "--bootstrap-shims".into(),
        shims.into_os_string(),
        "--".into(),
        "build".into(),
        "--config".into(),
        config.into_os_string(),
        "--src".into(),
        source.into_os_string(),
        "--build-dir".into(),
        output.clone().into_os_string(),
        "--stage".into(),
        "1".into(),
        "--jobs".into(),
        "2".into(),
        "native-object-leaf".into(),
    ];
    pending.submission.request.argv = args
        .iter()
        .map(|word| word.as_os_str().as_bytes().to_vec())
        .collect();
    pending.submission.request.cpu_units = 2;
    pending.submission.request.cwd = original_cwd;
    pending.submission.request.env = vec![
        (b"LANG".to_vec(), b"C".to_vec()),
        (b"LC_ALL".to_vec(), b"C".to_vec()),
        (
            b"CARGO".to_vec(),
            sdk.join("bin/targo").as_os_str().as_bytes().to_vec(),
        ),
        (b"RUSTC".to_vec(), compiler.as_os_str().as_bytes().to_vec()),
        (
            b"CARGO_HOME".to_vec(),
            cargo_home.as_os_str().as_bytes().to_vec(),
        ),
        (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
    ];
    pending.submission.fds.stdout = File::create(&stdout).unwrap().into();
    pending.submission.fds.stderr = File::create(&stderr).unwrap().into();
    pending.recipe = None;
    owner.recipe_layout = Some(layout.clone());
    assert_eq!(read_event(&events, &binding), Event::Queued);
    let pool = owner.pool.clone();
    let debt = owner.debt.clone();
    let ledger = owner.ledger.clone();
    let mut actor = Actor(Some(crate::actor::activate_test(owner).unwrap()));
    let mut connections = Vec::new();
    let until = Instant::now() + ROOT_LIMIT;
    let mut saw_paid = false;
    let mut saw_sdk = false;
    while !crate::actor::idle() {
        saw_paid |= debt.reserved().unwrap() == 2;
        saw_sdk |= matches!(
            atpkg::lease::holders(&layout.prefix, &subject),
            atpkg::lease::Holders::Held(_)
        );
        match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => {
                let image = program.clone();
                connections.push(std::thread::spawn(move || {
                    if let Err(why) = crate::runtime::serve_test(&stream, &image) {
                        eprintln!("actual graph transport ended: {why}");
                    }
                }));
            }
            Err(why)
                if matches!(
                    why.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(why) => panic!("actual graph listener: {why}"),
        }
        assert!(
            Instant::now() < until,
            "actual graph did not retire; stdout {}; stderr {}",
            stdout.display(),
            stderr.display()
        );
        std::thread::sleep(POLL);
    }
    actor.finish();
    for connection in connections {
        connection.join().unwrap();
    }
    let start = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    fs::write(artifacts.join("started.frame"), &start).unwrap();
    let Event::Started { pid, .. } = EventFrame::decode(&start, &binding)
        .unwrap_or_else(|why| {
            let terminal = AccountedEventFrame::decode(&start, &binding)
                .ok()
                .map(|frame| frame.finished.event);
            panic!(
                "actual Root not Started ({why}); terminal {terminal:?}; stderr {}",
                fs::read_to_string(&stderr).unwrap()
            )
        })
        .event
    else {
        panic!("actual Root not Started")
    };
    let terminal = wire::read_frame(&events, Instant::now() + HANG).unwrap();
    fs::write(artifacts.join("terminal.frame"), &terminal).unwrap();
    let finished = AccountedEventFrame::decode(&terminal, &binding).unwrap();
    let Event::Finished { outcome, cause, .. } = finished.finished.event else {
        panic!("actual Root did not finish")
    };
    assert_eq!(cause, CompletionCause::Exited);
    assert_eq!(
        outcome,
        Outcome::Exit(0),
        "actual Root stderr {}",
        fs::read_to_string(&stderr).unwrap()
    );
    assert!(finished.observation.cpu_times.is_some());
    assert!(saw_paid && saw_sdk);
    assert_eq!(
        process::process_presence(pid as i32),
        ProcessPresence::Absent
    );
    assert_eq!(debt.reserved().unwrap(), 0);
    assert_eq!(
        ledger.cohort_claim_state(&memory).unwrap(),
        CohortClaimState::Closed
    );
    assert!(
        pool.snapshot()
            .unwrap()
            .iter()
            .all(|slot| matches!(slot, aterm_capacity::build_cpu::SlotState::Available { .. }))
    );
    assert!(matches!(
        atpkg::lease::holders(&layout.prefix, &subject),
        atpkg::lease::Holders::Free
    ));
    let artifact = output.join("trust_bootstrap_native_object.o");
    let (file, metadata) = aterm_uds::regular_file::open(&artifact).unwrap();
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(metadata.mode() & 0o777, 0o400);
    assert_eq!(metadata.nlink(), 1);
    trust_resource_provenance::require_no_acl_grants(&file).unwrap();
    let mut bytes = Vec::new();
    let mut file = file;
    file.read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() >= 32);
    assert_eq!(&bytes[..4], &0xfeed_facfu32.to_le_bytes());
    assert_eq!(&bytes[4..8], &0x0100_000cu32.to_le_bytes());
    assert_eq!(&bytes[12..16], &1u32.to_le_bytes());
    assert!(
        fs::read_to_string(stdout)
            .unwrap()
            .contains("Native object produced at")
    );
    println!(
        "{SUCCESS} object_bytes={} object_sha256={} evidence={}",
        bytes.len(),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes)),
        artifacts.display()
    );
}
