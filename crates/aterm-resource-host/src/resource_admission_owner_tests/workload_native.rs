// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real private host/cohort/SCM custody with a closed native stand-in SDK.
//! This does not qualify real Targo compilation, UnitOutput or workload results.
use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_uds::{CtlListener, deadline};
use std::{
    fs,
    io::{Read as _, Write as _},
    os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _},
};
use trust_resource_controller::{
    self as control,
    workload::{self as wire, host},
};

const EXACT: &str = "actor::owner::tests::workload_native::actual_original_workload_root_and_phases_keep_all_resources_until_retirement";
const ROLE: &str = "ATERM_WORKLOAD_NATIVE_TEST_ROLE";
const STORE: &str = "ATERM_WORKLOAD_NATIVE_TEST_STORE";
const CASE: &str = "ATERM_WORKLOAD_NATIVE_TEST_CASE";
const LOCK_CHECKPOINT: [u8; 32] = [38; 32];
fn descriptor_population() -> (libc::rlim_t, usize) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    let count = (0..limit.rlim_cur.min(65536))
        .filter(|fd| unsafe { libc::fcntl(*fd as i32, libc::F_GETFD) } >= 0)
        .count();
    (limit.rlim_cur, count)
}

fn attempt_budget(full: bool) -> Duration {
    if full { Duration::from_secs(600) } else { HANG }
}
#[cfg(target_arch = "aarch64")]
#[path = "workload_native/contained.rs"]
mod contained;
#[repr(C, align(16))]
struct Marker([u8; 64]);
#[used]
#[unsafe(link_section = "__TEXT,__trust_workload")]
static ROOT_MARKER: Marker = Marker(wire::WORKLOAD_PROTOCOL_MARKER);
#[used]
#[unsafe(link_section = "__TEXT,__trust_member")]
static MEMBER_MARKER: Marker = Marker(wire::WORKLOAD_MEMBER_PROTOCOL_MARKER);
extern "C" fn entry() {
    #[cfg(target_arch = "aarch64")]
    if matches!(std::env::args_os().skip(1).collect::<Vec<_>>().as_slice(),[arg] if arg=="--list" || arg=="--test-threads=1")
        && std::env::args_os().next().is_some_and(|arg| {
            matches!(
                std::path::Path::new(&arg)
                    .file_name()
                    .and_then(|name| name.to_str()),
                Some("contained-harness-0" | "contained-harness-1")
            )
        })
    {
        std::process::exit(std::panic::catch_unwind(contained::target).unwrap_or(101));
    }
    if std::env::args_os().skip(1).collect::<Vec<_>>()
        != [OsString::from(wire::WORKLOAD_STARTUP_ARGUMENT)]
    {
        return;
    }
    let result = std::panic::catch_unwind(root);
    std::process::exit(result.unwrap_or(101));
}
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static ENTRY: extern "C" fn() = entry;

struct ActualActor(Option<std::thread::JoinHandle<()>>);
impl ActualActor {
    fn finish(&mut self) {
        assert!(crate::actor::stop_test().unwrap());
        self.0.take().unwrap().join().unwrap();
    }
}
impl Drop for ActualActor {
    fn drop(&mut self) {
        if self.0.is_none() {
            return;
        }
        let end = Instant::now() + HANG;
        while Instant::now() < end {
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

#[test]
fn actual_original_workload_root_and_phases_keep_all_resources_until_retirement() {
    if std::env::var(ROLE).as_deref() == Ok("host") {
        return host_process();
    }
    for case in ["normal", "nonzero", "unknown", "cancel"] {
        run_case(case);
    }
}
#[test]
#[cfg(target_arch = "aarch64")]
fn actual_complete_contained_population_retains_owners_with_original_fd_limit() {
    run_case("contained-full-population");
}
fn run_case(case: &str) {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("workload-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let path = scratch.path().canonicalize().unwrap().join("host");
    let store = ImageStore::prepare_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    let mut command = spawnfd::command(program.path());
    command
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, "host")
        .env(STORE, &path)
        .env(CASE, case);
    let output = match deadline::output_within(
        &mut command,
        attempt_budget(case == "contained-full-population"),
    ) {
        Ok(output) => output,
        Err(error) => panic!(
            "workload host {case} failed; preserved {}: {error:?}",
            scratch.keep().display()
        ),
    };
    assert!(
        output.status.success(),
        "actual workload host {case} failed; preserved {}: {output:?}",
        scratch.keep().display()
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("actual-workload-original-owner-retired")
    );
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("contained "))
    {
        println!("{line}");
    }
    for file in fs::read_dir(path.join("images")).unwrap() {
        fs::set_permissions(file.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap()
    }
}
fn input(path: &std::path::Path) -> wire::InputFile {
    wire::InputFile {
        path: path.as_os_str().as_bytes().to_vec(),
        sha256: aterm_digest::Sha256::digest(fs::read(path).unwrap()),
    }
}
fn host_process() {
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let case = std::env::var(CASE).unwrap();
    if case == "contained-full-population" {
        // This fresh private fixture gets a stricter real kernel limit BEFORE
        // any owners. Capping hard and soft prevents ordinary spawn's existing
        // soft-limit preparation from widening this controlled input.
        let limit = libc::rlimit {
            rlim_cur: 256,
            rlim_max: 256,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    }
    let budget = attempt_budget(case == "contained-full-population");
    let store = ImageStore::open_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    let socket = path.join("workload.sock");
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let _instance = store.publish(&program, &listener).unwrap();
    crate::runtime::test_endpoint_with_store(&path).unwrap();
    // Selected native inputs require private, non-writable ancestors. Keep
    // only the Unix socket name in the short temporary namespace; the owned
    // SDK/output/CPU fixture is independent account-cache data, never install.
    let parent = trust_resource_provenance::process::account_home()
        .unwrap()
        .join(".cache/aterm/workload-owner-fixtures.noindex");
    fs::create_dir_all(&parent).unwrap();
    let scratch = Scratch::at(&parent);
    let (mut owner, namespace, retirement) = World::prepare(&scratch);
    let artifacts = scratch.path.join("managed-sdk");
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
    fs::copy(program.path(), build.join("bin/rust-lld")).unwrap();
    atpkg::platform::install_shim_to(&layout.bin_dir().join("targo"), &service).unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    let source = scratch.path.join("source");
    let cargo_home = scratch.path.join("cargo-home");
    let output = scratch.path.join("output");
    for dir in [&source, &cargo_home, &output] {
        fs::create_dir(dir).unwrap()
    }
    let mut inputs = Vec::new();
    for name in ["step", "loop", "spec"] {
        let file = source.join(name);
        fs::write(&file, &case).unwrap();
        inputs.push(input(&file))
    }
    let invocation = wire::WorkloadInvocation {
        invocation: [33; 16],
        observed_host_store: Some(path.as_os_str().as_bytes().to_vec()),
        root: build.as_os_str().as_bytes().to_vec(),
        program: input(&service),
        compiler: input(&compiler),
        cwd: source.as_os_str().as_bytes().to_vec(),
        cargo_home: cargo_home.as_os_str().as_bytes().to_vec(),
        output_root: output.as_os_str().as_bytes().to_vec(),
        refutation_step: inputs.remove(0),
        refutation_loop: inputs.remove(0),
        trust_spec_source: inputs.remove(0),
        repetitions: if case == "contained-full-population" {
            32
        } else {
            1
        },
        // Full-population native actions use the one-minute hang detector within
        // their ten-minute attempt. Single-pass phases retain room for admission
        // and preparation inside their original whole-attempt minute.
        timeout_ns: if case == "contained-full-population" {
            HANG
        } else {
            Duration::from_secs(5)
        }
        .as_nanos() as u64,
    };
    let bytes = invocation.encode().unwrap();
    let (root_input, mut continuation) = std::io::pipe().unwrap();
    continuation
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    continuation.write_all(&bytes).unwrap();
    let (binding, memory, events) = queue_owned(&mut owner, &scratch.path, budget, [7; 16]);
    let pending = owner.pending.back_mut().unwrap();
    pending.events.account().unwrap();
    pending.submission.request.argv = vec![
        service.as_os_str().as_bytes().to_vec(),
        wire::WORKLOAD_STARTUP_ARGUMENT.as_bytes().to_vec(),
    ];
    pending.submission.request.cpu_units = 2;
    pending.submission.request.cwd = source.as_os_str().as_bytes().to_vec();
    pending.submission.request.env = vec![
        (b"LANG".to_vec(), b"C".to_vec()),
        (b"LC_ALL".to_vec(), b"C".to_vec()),
        (b"CARGO".to_vec(), invocation.program.path.clone()),
        (b"RUSTC".to_vec(), invocation.compiler.path.clone()),
        (b"CARGO_HOME".to_vec(), invocation.cargo_home.clone()),
        (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
    ];
    let stderr = scratch.path.join("stderr");
    pending.submission.fds.stdin = root_input.into();
    pending.submission.fds.stderr = File::create(&stderr).unwrap().into();
    pending.recipe = None;
    owner.recipe_layout = Some(layout.clone());
    assert_eq!(read_event(&events, &binding), Event::Queued);
    let pool = owner.pool.clone();
    let debt = owner.debt.clone();
    let ledger = owner.ledger.clone();
    let mut actor = ActualActor(Some(crate::actor::activate_test(owner).unwrap()));
    let mut connections = Vec::new();
    let end = Instant::now() + budget;
    let mut last_progress = Instant::now();
    let mut progress_bytes = 0;
    let mut reserved = false;
    let mut cancelled = false;
    let mut locks_observed = false;
    let (original_fd_limit, _) = descriptor_population();
    let mut fd_trace = File::create(scratch.path.join("host-descriptors")).unwrap();
    writeln!(fd_trace, "soft={original_fd_limit}").unwrap();
    let mut peak_descriptors = 0;
    while !crate::actor::idle() {
        let actual_bytes =
            fs::metadata(source.join("contained-descriptors")).map_or(0, |stat| stat.len());
        if actual_bytes != progress_bytes {
            progress_bytes = actual_bytes;
            last_progress = Instant::now();
            writeln!(
                fd_trace,
                "root_progress_bytes={actual_bytes} descriptors={} peak={peak_descriptors}",
                descriptor_population().1
            )
            .unwrap();
        }
        assert!(
            last_progress.elapsed() < HANG,
            "no actual fixture action progress within original hang detector"
        );
        let (actual_limit, actual_count) = descriptor_population();
        assert_eq!(actual_limit, original_fd_limit);
        peak_descriptors = peak_descriptors.max(actual_count);
        assert!(
            Instant::now() < end,
            "native original workload never settled: {}",
            fs::read_to_string(&stderr).unwrap()
        );
        if debt.reserved().unwrap() == 2 {
            reserved = true;
            assert!(
                pool.snapshot()
                    .unwrap()
                    .iter()
                    .all(|slot| matches!(slot, aterm_capacity::build_cpu::SlotState::Held { .. }))
            );
        }
        if !locks_observed && source.join("original-lock-dropped").is_file() {
            // Root has closed its original guard. Only the actual Host SCM
            // copy remains, and positive phase Leave must not release it.
            assert_eq!(
                aterm_uds::file_lock::LockFile::take_existing(
                    &output.join(".cargo-lock"),
                    Instant::now(),
                    "busy"
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::WouldBlock
            );
            locks_observed = true;
            if case != "cancel" {
                let ack = wire::Continuation {
                    invocation_sha256: aterm_digest::Sha256::digest(&bytes),
                    root_attempt: binding.attempt,
                    checkpoint: 1,
                    checkpoint_sha256: LOCK_CHECKPOINT,
                    decision: wire::Decision::Continue,
                }
                .encode()
                .unwrap();
                continuation
                    .write_all(&(ack.len() as u32).to_be_bytes())
                    .unwrap();
                continuation.write_all(&ack).unwrap();
            }
        }
        if case == "cancel" && !cancelled && source.join("ready").is_file() {
            assert!(crate::actor::control_eof_before_reap_test(binding.clone()).unwrap());
            aterm_ctl::resource_admission_transport::write_frame(
                &events,
                &aterm_types::resource_admission::Cancel {
                    binding: binding.clone(),
                }
                .encode()
                .unwrap(),
                end,
            )
            .unwrap();
            cancelled = true;
        }
        match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => {
                let image = program.clone();
                connections.push(std::thread::spawn(move || {
                    if let Err(error) = crate::runtime::serve_test(&stream, &image) {
                        eprintln!("actual workload connection ended: {error:?}");
                    }
                }));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => panic!("workload listener: {error}"),
        }
        std::thread::sleep(POLL);
    }
    assert!(
        reserved,
        "the actual full original pool must have been held"
    );
    actor.finish();
    for connection in connections {
        connection.join().unwrap()
    }
    let start = aterm_ctl::resource_admission_transport::read_frame(&events, Instant::now() + HANG)
        .unwrap();
    let started = EventFrame::decode(&start, &binding).unwrap_or_else(|error| {
        let refused =
            aterm_types::resource_admission::AccountedEventFrame::decode(&start, &binding)
                .ok()
                .map(|frame| frame.finished.event);
        panic!(
            "native workload did not start: {error}; actual terminal {refused:?}; stderr {}",
            fs::read_to_string(&stderr).unwrap()
        )
    });
    let Event::Started { pid, .. } = started.event else {
        panic!(
            "workload root never started: {}",
            fs::read_to_string(&stderr).unwrap()
        )
    };
    let finished = aterm_types::resource_admission::AccountedEventFrame::decode(
        &aterm_ctl::resource_admission_transport::read_frame(&events, Instant::now() + HANG)
            .unwrap(),
        &binding,
    )
    .unwrap();
    let Event::Finished { outcome, cause, .. } = finished.finished.event else {
        panic!("actual root terminal missing")
    };
    assert!(
        locks_observed,
        "actual transferred output lock was never observed held; original terminal {outcome:?}/{cause:?}; stderr {}",
        fs::read_to_string(&stderr).unwrap(),
    );
    if case == "unknown" || case == "cancel" {
        assert_eq!(
            cause,
            if case == "cancel" {
                CompletionCause::Cancelled
            } else {
                CompletionCause::HostFault
            }
        );
        assert!(matches!(outcome, Outcome::Signal(_)))
    } else {
        assert_eq!(cause, CompletionCause::Exited);
        assert_eq!(
            outcome,
            Outcome::Exit(if case == "nonzero" { 23 } else { 0 })
        );
        assert!(finished.observation.cpu_times.is_some())
    }
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
        atpkg::lease::holders(
            &layout.prefix,
            &atpkg::lease::Subject::build("trust", 1).unwrap()
        ),
        atpkg::lease::Holders::Free
    ));
    if case == "contained-full-population" {
        println!("contained host soft={original_fd_limit} peak={peak_descriptors}");
        print!(
            "{}",
            fs::read_to_string(source.join("contained-descriptors")).unwrap()
        );
    }
    let probe = aterm_uds::file_lock::LockFile::take_existing(
        &output.join(".cargo-lock"),
        Instant::now(),
        "busy",
    )
    .unwrap();
    drop(probe);
    println!("actual-workload-original-owner-retired case={case} cpu=0 memory=closed SDK=free");
    fs::remove_file(socket).unwrap();
    drop((pool, debt, ledger, namespace, retirement));
    drop(scratch);
}
fn root() -> i32 {
    // Snapshot before any allocation/import; these locators never grant CPU.
    let sdk = control::client::sdk::snapshot_sdk_lease().unwrap();
    let cpus = control::client::workload::snapshot_cpu_set().unwrap();
    let store = control::client::workload::snapshot_host_store().unwrap();
    assert!(cpus.is_present() && store.is_present());
    unsafe { control::client::sdk::capture_sdk_lease(sdk).unwrap() };
    let cpus = unsafe { control::client::workload::capture_cpu_set(cpus).unwrap() };
    let store = unsafe { control::client::workload::capture_host_store(store).unwrap() };
    let mut header = [0; 4];
    std::io::stdin().read_exact(&mut header).unwrap();
    let n = u32::from_be_bytes(header) as usize;
    assert!(n <= control::MAX_PAYLOAD);
    let mut raw = vec![0; n];
    std::io::stdin().read_exact(&mut raw).unwrap();
    let invocation = wire::WorkloadInvocation::decode(&raw).unwrap();
    let deadline = Instant::now() + attempt_budget(invocation.repetitions == 32);
    let client = store.discover(deadline).unwrap();
    assert_eq!(
        Some(client.original_store_path().as_os_str().as_bytes()),
        invocation.observed_host_store.as_deref()
    );
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let request = host::ReadyRequest {
        query: control::ContextQuery {
            binding,
            call_attempt: [34; 16],
        },
        invocation: invocation.clone(),
    };
    let startup = client.workload_ready(&request, cpus, deadline).unwrap();
    let (reply, originals, mut control) = startup.into_originals();
    assert_eq!(originals.len(), 2);
    assert_eq!(reply.cpu_set().count(), 2);
    control::client::sdk::validate_sdk_keeper(&reply.context.context).unwrap();
    let source = PathBuf::from(std::ffi::OsStr::from_bytes(&invocation.cwd));
    let case = fs::read_to_string(source.join("step")).unwrap();
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let output = PathBuf::from(std::ffi::OsStr::from_bytes(&invocation.output_root));
    let lock_path = output.join(".cargo-lock");
    let original_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "this original fixture owner must close without LOCK_UN so the received same-OFD copy retains the lock"
    )]
    original_lock.lock().unwrap();
    let metadata = original_lock.metadata().unwrap();
    let offer = host::output::Request {
        key: host::output::Key {
            root_attempt: request.query.binding.root_attempt,
            invocation_sha256: reply.invocation_sha256,
            attempt: [37; 16],
        },
        output_root: invocation.output_root.clone(),
        locks: vec![host::output::Lock {
            path: lock_path.as_os_str().as_bytes().to_vec(),
            backing: control::FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        }],
    };
    // SAFETY: this native stand-in owns its complete original fixture guard,
    // with close-only std File Drop, before any compiler dispatch. This tests
    // actual original Host transfer/retirement, not real Cargo Layout custody.
    let retained =
        unsafe { control.retain_output_locks(&offer, &[original_lock.as_fd()], deadline) }.unwrap();
    assert_eq!(retained.observation(), &offer);
    drop(original_lock);
    // A real paid Root with original CPU/SDK/output custody is still not a
    // registered parked compiler. Exercise independent native refusal before
    // any linker rights/target are accepted; do not forge a positive pause.
    // The public descriptor contract reserves sources below5. An arbitrary
    // File::try_clone can reuse a freed bootstrap slot, so deliberately copy
    // this SAME actual original OFD into the supported source range.
    let copied = unsafe { libc::fcntl(originals[0].as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
    assert!(copied >= 5);
    let offered = unsafe { OwnedFd::from_raw_fd(copied) };
    let linker = control::linker::Request {
        root: invocation.root.clone(),
        member: control::Request {
            binding: request.query.binding.clone(),
            member_attempt: [39; 16],
            role: control::Role::MachOLinker(control::Loan {
                parent_attempt: [40; 16],
                nonce: [41; 16],
                cpu_slot: reply.primary.slot,
            }),
            command: control::CommandSpec {
                program: [invocation.root.as_slice(), b"/bin/rust-lld"].concat(),
                arg0: None,
                args: [
                    b"-flavor".as_slice(),
                    b"darwin",
                    b"--threads=1",
                    b"--thinlto-jobs=1",
                    b"--read-workers=0",
                ]
                .into_iter()
                .map(Vec::from)
                .collect(),
                environment: vec![
                    (b"LANG".to_vec(), b"C".to_vec()),
                    (b"LC_ALL".to_vec(), b"C".to_vec()),
                ],
                cwd: invocation.cwd.clone(),
            },
            delegations: control::Delegations {
                cpu: Some(control::Descriptor {
                    source: offered.as_raw_fd() as u32,
                    target: 5,
                }),
                gnu: control::GnuTransport::None,
                fences: Vec::new(),
            },
            timeout_ns: Some(1_000_000_000),
        },
    };
    linker.validate().unwrap();
    // Intentional adversarial violation of the SDK pause precondition: every
    // actual descriptor is valid/owned, but no compiler member exists. The
    // independently authenticating Host must refuse this logical forgery.
    let refused = unsafe {
        client.linker_loan(
            &linker,
            control::client::MetadataIo {
                descriptions: vec![offered],
            },
            deadline,
        )
    };
    if !matches!(&refused, Err(control::client::LinkerError::Refused)) {
        let reason = match &refused {
            Err(control::client::LinkerError::BeforeRequest(error)) => {
                format!("before request: {error}")
            }
            Err(control::client::LinkerError::Unknown { error, .. }) => {
                format!("unknown delivery: {error}")
            }
            _ => "unexpected positive attachment".into(),
        };
        panic!("native Root forged compiler refusal unavailable: {reason}");
    }
    if matches!(case.as_str(), "unknown" | "cancel") {
        fs::write(source.join("original-lock-dropped"), "actual local close").unwrap();
    }
    fs::write(source.join("ready"), "actual checked original root").unwrap();
    if case == "unknown" {
        // Await the independent writer probe, rather than racing retirement.
        std::io::stdin().read_exact(&mut header).unwrap();
        let n = u32::from_be_bytes(header) as usize;
        assert!(n <= control::MAX_PAYLOAD);
        let mut ack = vec![0; n];
        std::io::stdin().read_exact(&mut ack).unwrap();
        let ack = wire::Continuation::decode(&ack).unwrap();
        ack.check_binding(
            reply.invocation_sha256,
            request.query.binding.root_attempt,
            1,
            LOCK_CHECKPOINT,
        )
        .unwrap();
        assert_eq!(ack.decision, wire::Decision::Continue);
        drop(originals);
        drop(control);
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
    if case == "cancel" {
        loop {
            std::hint::black_box(&control);
            std::hint::black_box(&originals);
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
    let fifo_path = source.join("original-gnu");
    let cpath = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let read = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo_path)
        .unwrap();
    let write = fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo_path)
        .unwrap();
    let stat = read.metadata().unwrap();
    let backing = control::FileIdentity {
        device: stat.dev(),
        inode: stat.ino(),
    };
    let key = |sequence| host::bank::Key {
        root_attempt: request.query.binding.root_attempt,
        invocation_sha256: reply.invocation_sha256,
        sequence,
    };
    let enter = host::phase::Request {
        key: key(1),
        operation: host::phase::Operation::Enter {
            phase: [35; 16],
            kind: host::phase::Kind::Harnesses,
            gnu: backing,
        },
    };
    let accepted = control
        .request_phase(&enter, Some([read.as_fd(), write.as_fd()]), deadline)
        .unwrap();
    assert!(matches!(
        accepted.decision,
        host::phase::Decision::Ready {
            reserved_domain: 2,
            ..
        }
    ));
    let action_count: u8 = 68;
    #[cfg(target_arch = "aarch64")]
    let _contained = (case == "contained-full-population").then(|| {
        contained::run(
            &client,
            &mut control,
            &request,
            reply.invocation_sha256,
            backing,
            deadline,
            action_count,
        )
    });
    let mut sequence = 2;
    loop {
        let left = control
            .request_phase(
                &host::phase::Request {
                    key: key(sequence),
                    operation: host::phase::Operation::Leave { phase: [35; 16] },
                },
                None,
                deadline,
            )
            .unwrap();
        sequence += 1;
        match left.decision {
            host::phase::Decision::Left { .. } => break,
            host::phase::Decision::Busy => {
                assert!(
                    Instant::now() < deadline,
                    "original members must actually retire before Leave"
                );
                std::thread::sleep(POLL);
            }
            other => panic!("unexpected original phase Leave: {other:?}"),
        }
    }
    fs::write(
        source.join("original-lock-dropped"),
        "actual close plus positive Leave",
    )
    .unwrap();
    // This fixture-only continuation gates inspection of the already-owned
    // lock; it is not a compilation checkpoint or an execution authorization.
    std::io::stdin().read_exact(&mut header).unwrap();
    let n = u32::from_be_bytes(header) as usize;
    assert!(n <= control::MAX_PAYLOAD);
    let mut ack = vec![0; n];
    std::io::stdin().read_exact(&mut ack).unwrap();
    let ack = wire::Continuation::decode(&ack).unwrap();
    ack.check_binding(
        reply.invocation_sha256,
        request.query.binding.root_attempt,
        1,
        LOCK_CHECKPOINT,
    )
    .unwrap();
    assert_eq!(ack.decision, wire::Decision::Continue);
    let drained = control
        .request_phase(
            &host::phase::Request {
                key: key(sequence),
                operation: host::phase::Operation::Drained,
            },
            None,
            deadline,
        )
        .unwrap();
    assert!(matches!(drained.decision, host::phase::Decision::Drained));
    let mut census = control.census_snapshot([36; 16], deadline).unwrap();
    let observed = census.next_page(deadline).unwrap().unwrap();
    if case == "contained-full-population" {
        assert_eq!(
            observed.len(),
            usize::from(action_count),
            "complete real admitted action denominator"
        );
        assert!(
            observed
                .iter()
                .all(|entry| entry.retired && entry.drained && entry.started.is_some())
        );
    } else {
        assert!(observed.is_empty());
    }
    assert_eq!(census.next_page(deadline).unwrap(), None);
    assert!(census.complete());
    drop(census);
    drop(control);
    drop(originals);
    if case == "nonzero" { 23 } else { 0 }
}
