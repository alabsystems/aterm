// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dependency-qualified Host test, not a paired compiler or codegen-join claim.
//! The selected compiler stand-in has one main thread and no producing workers;
//! it blocks synchronously while its original primary is lent to genuine LLD.
//! Selected artifact provenance: maintained tests at Trust source
//! 54e7073a27246641e0c99077dc250a395d91d121; closed descendant receipt SHA256
//! 8f95ae4faf5976106e5167f7c447fdaf0a373031fe4e4c8ce5945a04631ff7e9.
//! The original 19e34990 artifact is preserved: its external RPATH correctly
//! refused Host SDK capture. The independently signed closed descendant has
//! zero RPATHs and fourteen identical file-backed section payloads.
//! The independent receipt binds the source/readset, exact feature JSON,
//! sole-writer EOF control and arm64 object/header smoke. Version alone does not.
//! Select its artifact with `ATERM_TEST_NATIVE_LLD` and its llvm-mc object with
//! `ATERM_TEST_NATIVE_LLD_OBJECT`, then run the ignored qualification explicitly:
//! ```text
//! build-slot -- targo --unverified test -p aterm-resource-host --lib actual_registered_compiler_direct_lld_retirement -- --ignored --nocapture
//! ```
//! This establishes no paired compiler SDK, LLVM join, hard quota, Linux
//! runtime, global adoption or complete v3 activation.
use super::*;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

const ARTIFACT: &str = "ATERM_TEST_NATIVE_LLD";
const SHA256: &str = "d7249a1da229a2f2b77488d7f286ef8edfe909be3a91600608ff7243387c566a";
const OBJECT: &str = "ATERM_TEST_NATIVE_LLD_OBJECT";
const OBJECT_SHA256: &str = "c716ed15a37dc7af2ad94356e8f01409faf014bc5ef1416ddb65b7ec7e7a61f7";
const PARENT: [u8; 16] = [71; 16];
const CHILD: [u8; 16] = [72; 16];
const NONCE: [u8; 16] = [73; 16];
const BANK_LOAN: [u8; 16] = [74; 16];
const CRATE: &str = "host_real_lld_standin";

pub(super) fn selected(case: &str) -> bool {
    matches!(case, "real-lld" | "real-lld-eof")
}

impl Owner {
    pub(in crate::actor) fn close_linker_lifeline_test(
        &mut self,
        binding: &Binding,
        attempt: [u8; 16],
    ) -> bool {
        self.jobs
            .get_mut(&(binding.session.clone(), binding.attempt))
            .and_then(|job| job.members.get_mut(&attempt))
            .is_some_and(|member| member.close_linker_lifeline())
    }
}

pub(super) fn close_writer(binding: Binding) {
    assert!(crate::actor::close_linker_lifeline_test(binding, CHILD).unwrap());
}

fn pipe_info(pid: u32, fd: i32) -> Option<aterm_sysprobe::jobs::darwin::PipeFd> {
    use aterm_sysprobe::jobs::darwin;
    let mut bytes = [0; darwin::PIPE_SIZE];
    // SAFETY: bounded read-only native descriptor observation of our fixture.
    let n = unsafe {
        libc::proc_pidfdinfo(
            pid as i32,
            fd,
            darwin::PROC_PIDFDPIPEINFO,
            bytes.as_mut_ptr().cast(),
            bytes.len() as i32,
        )
    };
    (n == bytes.len() as i32).then(|| darwin::pipe(&bytes).unwrap())
}

fn pipe_writers(pid: u32, read: aterm_sysprobe::jobs::darwin::PipeFd) -> usize {
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        4097
    ];
    let size = std::mem::size_of_val(fds.as_slice());
    // SAFETY: bounded native table of plain integer records, read-only query.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            size as i32,
        )
    };
    assert!(
        n > 0 && (n as usize) < size,
        "complete private fixture FD table required"
    );
    fds.truncate(n as usize / std::mem::size_of::<libc::proc_fdinfo>());
    fds.iter()
        .filter_map(|fd| pipe_info(pid, fd.proc_fd))
        .filter(|pipe| pipe.write_end() && pipe.handle == read.peer && pipe.peer == read.handle)
        .count()
}

fn lifeline(pid: u32, other_peers: &[u32]) {
    use aterm_sysprobe::jobs::darwin;
    let argv = process::read_process_argv(pid as i32).unwrap();
    assert!(argv.ends_with(&[
        OsString::from("--threads=1"),
        OsString::from("--thinlto-jobs=1"),
        OsString::from("--read-workers=0"),
    ]));
    assert!(
        !argv.iter().any(|arg| {
            let bytes = arg.as_bytes();
            let option = bytes
                .strip_prefix(b"--")
                .or_else(|| bytes.strip_prefix(b"-"));
            bytes == b"--"
                || bytes.starts_with(b"@")
                || option.is_some_and(|option| {
                    [b"filelist".as_slice(), b"mllvm", b"load-pass-plugin"]
                        .iter()
                        .any(|name| {
                            option == *name
                                || option
                                    .strip_prefix(*name)
                                    .is_some_and(|tail| tail.starts_with(b"="))
                        })
                })
        }),
        "actual final caps must be parsed without opaque option escapes"
    );
    let args: Vec<_> = argv
        .iter()
        .filter_map(|arg| arg.to_str()?.strip_prefix("--resource-lifeline-fd="))
        .collect();
    assert_eq!(
        args.len(),
        1,
        "actual LLD must receive exactly one Host-selected lifeline"
    );
    let fd: i32 = args[0].parse().unwrap();
    assert!(fd >= 3 && fd != 5);
    let read = pipe_info(pid, fd).expect("actual LLD lifeline must be an anonymous pipe");
    assert_eq!(
        read.openflags & (darwin::FREAD | darwin::FWRITE),
        darwin::FREAD
    );
    assert_ne!(read.openflags & libc::O_NONBLOCK as u32, 0);
    assert_eq!(read.queued, 0);
    assert_eq!(
        pipe_writers(std::process::id(), read),
        1,
        "H owns the sole writer"
    );
    for peer in std::iter::once(pid).chain(other_peers.iter().copied()) {
        assert_eq!(
            pipe_writers(peer, read),
            0,
            "no lifeline writer in target/compiler/Root"
        );
    }
}

fn sdk_guard(pid: u32, sdk: &aterm_json::Value) {
    use aterm_sysprobe::jobs::darwin;
    let path =
        aterm_codec::hex::decode_vec(sdk["lease_path_hex"].as_str().unwrap().as_bytes()).unwrap();
    // Equality observation of the original keeper's path, never a path-opened
    // SDK lease or a constructor of execution authority.
    let expected = fs::metadata(std::ffi::OsStr::from_bytes(&path)).unwrap();
    let mut bytes = [0; darwin::VNODE_PATH_SIZE];
    // SAFETY: bounded read-only kernel observation of our actual LLD's SDK7.
    let n = unsafe {
        libc::proc_pidfdinfo(
            pid as i32,
            7,
            darwin::PROC_PIDFDVNODEPATHINFO,
            bytes.as_mut_ptr().cast(),
            bytes.len() as i32,
        )
    };
    assert_eq!(n, bytes.len() as i32);
    let actual = darwin::vnode(&bytes).unwrap();
    assert!(actual.regular());
    assert_eq!(
        (u64::from(actual.dev), actual.ino),
        (expected.dev(), expected.ino())
    );
    println!("real-lld sdk7 device={} inode={}", actual.dev, actual.ino);
}

pub(super) fn loan_is_parked(
    namespace: &Namespace,
    binding: &Binding,
    selected: &std::path::Path,
) -> bool {
    let active = namespace
        .backend()
        .parent()
        .unwrap()
        .join("resource-host-v3.noindex/active");
    let path = active.join(format!(
        "{}.{}.json",
        binding.session,
        aterm_codec::hex::encode_lower(&PARENT)
    ));
    // Observe one atomically replaced original record. It does not exist before
    // registration; directory staging during other publications is irrelevant.
    match fs::read(path) {
        Ok(bytes) => {
            let row: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
            if row["cpu_loan"]["state"].as_str() != Some("loaned") {
                return false;
            }
            let child = active.join(format!(
                "{}.{}.json",
                binding.session,
                aterm_codec::hex::encode_lower(&CHILD)
            ));
            let child: aterm_json::Value =
                aterm_json::from_slice(&fs::read(child).unwrap()).unwrap();
            let Some(pid) = child["pid"].as_u64() else {
                return false;
            };
            let pid = u32::try_from(pid).unwrap();
            let Some(image) = process::LoadedImage::read(pid as i32) else {
                return false;
            };
            if image.matches_file(selected) != Some(true) {
                return false;
            }
            let lineage = process::read_lineage(pid as i32).unwrap();
            assert_eq!(lineage.identity.birth.0, child["birth"].as_u64().unwrap());
            assert_eq!(
                lineage.parent,
                std::process::id(),
                "H must remain actual LLD parent"
            );
            assert!(crate::metadata_prelude::no_fork(pid).unwrap());
            let parent = row["pid"].as_u64().unwrap() as u32;
            let root = active.join(format!(
                "{}.{}.json",
                binding.session,
                aterm_codec::hex::encode_lower(&binding.attempt)
            ));
            let root: aterm_json::Value = aterm_json::from_slice(&fs::read(root).unwrap()).unwrap();
            sdk_guard(pid, &root["sdk"]);
            lifeline(pid, &[parent, root["root_pid"].as_u64().unwrap() as u32]);
            println!(
                "real-lld parked parent={} child={} native_pid={pid} host_pid={} no_fork=true",
                aterm_codec::hex::encode_lower(&PARENT),
                aterm_codec::hex::encode_lower(&CHILD),
                std::process::id()
            );
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => panic!("actual parent journal: {error}"),
    }
}

pub(super) fn release_input(source: &std::path::Path) -> bool {
    let Some(mut writer) = hold_input(source) else {
        return false;
    };
    let bytes =
        fs::read(std::env::var_os(OBJECT).expect("set ATERM_TEST_NATIVE_LLD_OBJECT")).unwrap();
    assert_eq!(
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes)),
        OBJECT_SHA256
    );
    writer.write_all(&bytes).unwrap();
    true
}

pub(super) fn hold_input(source: &std::path::Path) -> Option<File> {
    match fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(source.join("linker-input"))
    {
        Ok(writer) => Some(writer),
        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => None,
        Err(error) => panic!("actual blocked LLD input: {error}"),
    }
}

pub(super) fn retired_receipt(namespace: &Namespace, source: &std::path::Path) {
    let done = namespace
        .backend()
        .parent()
        .unwrap()
        .join("resource-host-v3.noindex/done");
    let parent = aterm_codec::hex::encode_lower(&PARENT);
    let records: Vec<aterm_json::Value> = fs::read_dir(done)
        .unwrap()
        .map(|entry| aterm_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect();
    let record = records
        .iter()
        .find(|row| row["attempt"].as_str() == Some(parent.as_str()))
        .unwrap();
    assert_eq!(record["cpu_loan"]["state"].as_str(), Some("returned"));
    assert_eq!(
        record["cpu_loan"]["child"].as_str(),
        Some(aterm_codec::hex::encode_lower(&CHILD).as_str())
    );
    println!("real-lld durable-parent={record}");
    let output = source.parent().unwrap().join("output/linked-output");
    if output.is_file() {
        println!(
            "real-lld arm64-output sha256={}",
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
                fs::read(output).unwrap()
            ))
        );
    }
    println!(
        "real-lld completion={}",
        fs::read_to_string(source.join("linker-retired")).unwrap()
    );
}

pub(super) fn install(build: &std::path::Path) {
    let artifact = PathBuf::from(std::env::var_os(ARTIFACT).expect("set ATERM_TEST_NATIVE_LLD"));
    let bytes = fs::read(&artifact).unwrap();
    assert_eq!(
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes)),
        SHA256
    );
    let selected = build.join("bin/rust-lld");
    fs::write(&selected, bytes).unwrap();
    fs::set_permissions(selected, fs::Permissions::from_mode(0o500)).unwrap();
}

pub(super) fn early_entry() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args
        .first()
        .is_some_and(|arg| arg == control::WorkloadHelperFds::ENTRY)
    {
        let result = crate::native_fixture::entry(&args);
        std::process::exit(if result == std::process::ExitCode::SUCCESS {
            0
        } else {
            101
        });
    }
    if matches!(args.as_slice(), [worker, subgroups, name, value, kind]
        if worker == "--resource-worker-v3" && subgroups == "--resource-subgroups-v3"
            && name == "--crate-name" && value == CRATE && kind == "--crate-type=rlib")
    {
        let result = std::panic::catch_unwind(compiler);
        std::process::exit(result.unwrap_or(101));
    }
}

fn copy(fd: &impl std::os::fd::AsRawFd) -> OwnedFd {
    let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 6) };
    assert!(raw >= 6);
    unsafe { OwnedFd::from_raw_fd(raw) }
}

pub(super) fn run(
    client: &control::client::Client,
    channel: &mut control::client::workload::WorkloadControl,
    ready: &host::ReadyRequest,
    reply: &host::ReadyReply,
    originals: &[File],
    deadline: Instant,
) {
    let invocation = &ready.invocation;
    let source = PathBuf::from(std::ffi::OsStr::from_bytes(&invocation.cwd));
    // This unused FIFO is the original shared scheduling context. There are no
    // supplemental tokens or worker threads in the controlled compiler recipe.
    let fifo = source.join("compiler-gnu");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let read = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let write = fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let stat = read.metadata().unwrap();
    let cpu = copy(&originals[0]);
    let read = copy(&read);
    let write = copy(&write);
    let request = control::Request {
        binding: ready.query.binding.clone(),
        member_attempt: PARENT,
        role: control::Role::Compile,
        command: control::CommandSpec {
            program: invocation.compiler.path.clone(),
            arg0: None,
            args: vec![
                b"--crate-name".to_vec(),
                CRATE.as_bytes().to_vec(),
                b"--crate-type=rlib".to_vec(),
            ],
            environment: vec![
                (b"LANG".to_vec(), b"C".to_vec()),
                (b"LC_ALL".to_vec(), b"C".to_vec()),
                (b"CARGO".to_vec(), invocation.program.path.clone()),
                (b"RUSTC".to_vec(), invocation.compiler.path.clone()),
                (b"CARGO_HOME".to_vec(), invocation.cargo_home.clone()),
                (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
            ],
            cwd: invocation.cwd.clone(),
        },
        delegations: control::Delegations {
            cpu: Some(control::Descriptor {
                source: cpu.as_raw_fd() as u32,
                target: 80,
            }),
            gnu: control::GnuTransport::NamedFifo {
                locator: fifo.as_os_str().as_bytes().to_vec(),
                device: stat.dev(),
                inode: stat.ino(),
                read: control::Descriptor {
                    source: read.as_raw_fd() as u32,
                    target: 82,
                },
                write: control::Descriptor {
                    source: write.as_raw_fd() as u32,
                    target: 83,
                },
            },
            fences: Vec::new(),
        },
        timeout_ns: Some(HANG.as_nanos() as u64),
    };
    // Member attachment waits for the bank, so the original broker remains on
    // this main thread. Only the proxy transport runs on a separate thread.
    std::thread::scope(|scope| {
        let request_ref = &request;
        let task = scope.spawn(move || {
            let mut child = client
                .workload_member_accounted(
                    request_ref,
                    control::client::MemberIo {
                        stdin: std::process::Stdio::null(),
                        stdout: std::process::Stdio::null(),
                        stderr: std::process::Stdio::inherit(),
                        descriptions: vec![cpu, read, write],
                    },
                    deadline,
                )
                .unwrap();
            loop {
                assert!(Instant::now() < deadline);
                if let Some(status) = child.try_wait(deadline) {
                    assert!(
                        matches!(
                            status,
                            control::Status::Finished {
                                outcome: control::Outcome::Exit(0),
                                cause: control::CompletionCause::Exited,
                                ..
                            }
                        ),
                        "compiler terminal {status:?}"
                    );
                    assert!(child.started().is_some());
                    assert!(child.accounting().is_some());
                    break;
                }
                std::thread::sleep(POLL);
            }
        });
        let mut borrowed = false;
        let mut returned = false;
        while !task.is_finished() {
            assert!(Instant::now() < deadline);
            if let Some(request) = channel.poll_bank_request(deadline).unwrap() {
                let decision = match request.operation {
                    host::bank::Operation::Primary {
                        member_attempt,
                        slot,
                    } => {
                        assert_eq!(member_attempt, PARENT);
                        assert_eq!(slot, reply.primary.slot);
                        assert!(!borrowed && !returned);
                        borrowed = true;
                        host::bank::Decision::Granted {
                            member_attempt,
                            slot,
                            loan: BANK_LOAN,
                            backing: reply.primary.backing,
                        }
                    }
                    host::bank::Operation::Returned {
                        member_attempt,
                        loan,
                        slot,
                    } => {
                        assert_eq!(
                            (member_attempt, loan, slot),
                            (PARENT, BANK_LOAN, reply.primary.slot)
                        );
                        assert!(borrowed && !returned);
                        assert!(source.join("linker-retired").is_file());
                        returned = true;
                        host::bank::Decision::Returned
                    }
                    host::bank::Operation::Extra { .. } => {
                        panic!("stand-in cannot request an extra CPU")
                    }
                };
                let original = matches!(decision, host::bank::Decision::Granted { .. })
                    .then(|| originals[0].as_fd());
                channel
                    .reply_bank(
                        &host::bank::Reply {
                            key: request.key,
                            decision,
                        },
                        original,
                        deadline,
                    )
                    .unwrap();
            }
            std::thread::sleep(POLL);
        }
        task.join().unwrap();
        assert!(
            borrowed && returned,
            "exact original primary must positively return"
        );
    });
    assert!(source.join("linker-retired").is_file());
}

fn compiler() -> i32 {
    let sdk = control::client::sdk::snapshot_sdk_lease().unwrap();
    let cpus = control::client::workload::snapshot_cpu_set().unwrap();
    let store = control::client::workload::snapshot_host_store().unwrap();
    assert!(cpus.is_present() && store.is_present());
    let gnu = jobserver_utils::recover_named_fifo_resources(
        &std::env::var_os("CARGO_JOBSERVER_HANDOFF").unwrap(),
    )
    .unwrap();
    unsafe { control::client::sdk::capture_sdk_lease(sdk).unwrap() };
    let cpus = unsafe { control::client::workload::capture_cpu_set(cpus).unwrap() };
    let store = unsafe { control::client::workload::capture_host_store(store).unwrap() };
    let deadline = Instant::now() + HANG;
    let client = store.discover(deadline).unwrap();
    assert!(crate::metadata_prelude::no_fork(std::process::id()).unwrap());
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let original = client
        .workload_member_original(
            &host::MemberOriginalRequest {
                query: control::ContextQuery {
                    binding: binding.clone(),
                    call_attempt: [75; 16],
                },
                member_attempt: PARENT,
            },
            cpus,
            deadline,
        )
        .unwrap();
    let (reply, primary) = original.into_original();
    let compiler = PathBuf::from(std::env::var_os("RUSTC").unwrap());
    let root = compiler.parent().unwrap().parent().unwrap();
    let offered = copy(&primary);
    let name = std::ffi::CString::new("linker-input").unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let request = control::linker::Request {
        root: root.as_os_str().as_bytes().to_vec(),
        member: control::Request {
            binding,
            member_attempt: CHILD,
            role: control::Role::MachOLinker(control::Loan {
                parent_attempt: PARENT,
                nonce: NONCE,
                cpu_slot: reply.cpu.slot,
            }),
            command: control::CommandSpec {
                program: root.join("bin/rust-lld").as_os_str().as_bytes().to_vec(),
                arg0: None,
                // Genuine LLD blocks on this private input until the Host
                // fixture observes its actual durable parked loan and retained
                // owners, then supplies the independently assembled arm64
                // object bound by the same qualification receipt.
                // Final limits remain actual parsed options, without a terminator.
                args: [
                    "-flavor",
                    "darwin",
                    "-arch",
                    "arm64",
                    "-platform_version",
                    "macos",
                    "14.0",
                    "14.0",
                    "-e",
                    "_start",
                    "-o",
                    "../output/linked-output",
                    "linker-input",
                    "--threads=1",
                    "--thinlto-jobs=1",
                    "--read-workers=0",
                ]
                .map(|word| word.as_bytes().to_vec())
                .to_vec(),
                environment: vec![
                    (b"LANG".to_vec(), b"C".to_vec()),
                    (b"LC_ALL".to_vec(), b"C".to_vec()),
                ],
                cwd: std::env::current_dir()
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
            },
            delegations: control::Delegations {
                cpu: Some(control::Descriptor {
                    source: offered.as_raw_fd() as u32,
                    target: 5,
                }),
                gnu: control::GnuTransport::None,
                fences: Vec::new(),
            },
            timeout_ns: Some(HANG.as_nanos() as u64),
        },
    };
    // SAFETY: this registered selected-image stand-in has only this main
    // thread, no producing work or callbacks, and retains its original CPU,
    // GNU and SDK throughout the synchronous loan. No decoded pause is used.
    let mut linker = unsafe {
        client.linker_loan(
            &request,
            control::client::MetadataIo {
                descriptions: vec![offered],
            },
            deadline,
        )
    }
    .unwrap();
    let result = loop {
        assert!(Instant::now() < deadline);
        if let Some(result) = linker.poll(deadline).unwrap() {
            break result;
        }
        std::thread::sleep(POLL);
    };
    let control::client::LinkerOutcome::Completed(result) = result else {
        panic!("actual LLD refused")
    };
    assert_eq!(
        result.prepared.executable.sha256,
        aterm_digest::Sha256::digest(fs::read(root.join("bin/rust-lld")).unwrap())
    );
    assert_eq!(result.prepared.loan, request.loan().unwrap());
    assert_eq!(result.started.identity, result.prepared.identity);
    assert_eq!(result.completion.loan, request.loan().unwrap());
    assert!(result.completion.observation.cpu_times.is_some());
    assert!(result.completion.observation.peak_rss_bytes.is_some());
    let eof = fs::read_to_string("step").unwrap() == "real-lld-eof";
    let expected = if eof { 75 } else { 0 };
    assert!(matches!(
        result.completion.completed.finished.status,
        control::Status::Finished {
            outcome: control::Outcome::Exit(code),
            cause: control::CompletionCause::Exited,
            ..
        } if code == expected
    ));
    assert!(!result.output_limit_exceeded);
    if eof {
        assert!(!std::path::Path::new("../output/linked-output").exists());
    } else {
        assert!(
            result.stderr.is_empty(),
            "{:?}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output = fs::read("../output/linked-output").unwrap();
        assert!(output.len() >= 32);
        assert_eq!(
            u32::from_le_bytes(output[0..4].try_into().unwrap()),
            0xfeedfacf
        );
        assert_eq!(
            u32::from_le_bytes(output[4..8].try_into().unwrap()),
            0x0100000c
        );
        assert_eq!(u32::from_le_bytes(output[12..16].try_into().unwrap()), 2);
        println!(
            "actual-linked-arm64-header sha256={}",
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&output))
        );
    }
    assert_eq!(
        process::process_presence(result.started.identity.pid as i32),
        ProcessPresence::Absent
    );
    fs::write("linker-retired", format!("{result:?}")).unwrap();
    drop((linker, primary, gnu));
    0
}
