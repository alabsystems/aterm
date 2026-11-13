// Trust: compiled controls for actual endpoint transport, not receipt strings.
use std::fs::File;

use super::*;

pub(super) fn cpu_file() -> (PathBuf, OwnedFd) {
    let path = std::env::temp_dir().join(format!("tjs-cpu-{}", hex(&random().unwrap()[..8])));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    writeln!(file, "trust-build-cpu-v1 slot=0 pid={} birth=- held=1", std::process::id()).unwrap();
    (path, file.into())
}

#[test]
fn cpu_handoff_rejects_other_file_kinds_and_records() {
    let [read, write] = pipe();
    assert!(CpuLeaseHandoff::new(read).is_err());
    drop(write);
    let (path, fd) = cpu_file();
    std::fs::write(&path, "ordinary private file").unwrap();
    assert!(CpuLeaseHandoff::new(fd).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cpu_and_jobserver_rights_have_distinct_roles() {
    let (path, fd) = cpu_file();
    let pair = [fd.try_clone().unwrap(), fd];
    let (left, right) = streams();
    send_pipe(&left, &pair, Instant::now() + TIMEOUT).unwrap();
    assert!(receive_pipe(&right, Instant::now() + TIMEOUT).is_err());
    let (left, right) = streams();
    send_pipe(&left, &pipe(), Instant::now() + TIMEOUT).unwrap();
    assert!(receive_role(&right, Instant::now() + TIMEOUT, Role::CpuLease).is_err());
    drop(pair);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cpu_endpoint_rejects_a_jobserver_role_before_transferring_rights() {
    let (path, fd) = cpu_file();
    let pair = [fd.try_clone().unwrap(), fd];
    let numbers = [pair[0].as_raw_fd(), pair[1].as_raw_fd()];
    let (mut client, server) = streams();
    let root = Process::current().unwrap();
    let worker = std::thread::spawn(move || {
        serve_role(server, &root, &pair, &[9; 32], numbers, Role::CpuLease)
    });
    let mut request = [0; 80];
    request[..8].copy_from_slice(MAGIC);
    request[8..40].copy_from_slice(&[9; 32]);
    request[72..76].copy_from_slice(&numbers[0].to_be_bytes());
    request[76..80].copy_from_slice(&numbers[1].to_be_bytes());
    write_frame(&mut client, &request, Instant::now() + TIMEOUT).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(worker.join().unwrap().unwrap_err().to_string().contains("session or pool mismatch"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cpu_handoff_descendant_recovers_without_using_advertised_numbers() {
    const CHILD: &str = "TJS_CPU_HANDOFF_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let value = std::env::var_os(super::super::CPU_HANDOFF_ENV).unwrap();
        let ad = Advertisement::parse(&value).unwrap();
        let owned = recover_cpu_lease(&value).unwrap();
        validate_cpu_file(&owned).unwrap();
        for fd in ad.pair {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: the parent deliberately passed these /dev/null descriptors.
            assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
            // SAFETY: the successful fstat initialized the complete value.
            assert_eq!(unsafe { stat.assume_init() }.st_mode & libc::S_IFMT, libc::S_IFCHR);
        }
        return;
    }
    let (path, fd) = cpu_file();
    let endpoint = Arc::new(CpuLeaseHandoff::new(fd).unwrap());
    let ad = Advertisement::parse(endpoint.advertisement()).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "handoff::tests::cpu_handoff_descendant_recovers_without_using_advertised_numbers",
            "--nocapture",
        ])
        .env(CHILD, "1");
    endpoint.configure(&mut child);
    use std::os::unix::process::CommandExt;
    // SAFETY: dup2 is async-signal-safe. It changes only the fork child's copies
    // of these descriptor slots; the parent's CPU lease remains untouched.
    unsafe {
        child.pre_exec(move || {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
            if null < 0 {
                return Err(io::Error::last_os_error());
            }
            for fd in ad.pair {
                if libc::dup2(null, fd) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if !ad.pair.contains(&null) {
                libc::close(null);
            }
            Ok(())
        });
    }
    let output = child.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    drop(endpoint);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cpu_bundle_preserves_actual_fence_locks_after_parent_copies_close() {
    const CHILD: &str = "TJS_CPU_BUNDLE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let value = std::env::var_os(super::super::CPU_HANDOFF_ENV).unwrap();
        let (primary, fences) = recover_cpu_resources(&value).unwrap();
        validate_cpu_file(&primary).unwrap();
        assert_eq!(fences.len(), 1);
        validate_fence_file(&fences[0]).unwrap();
        println!("BUNDLE_READY");
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_exact(&mut [0]).unwrap();
        return;
    }
    let (path, primary) = cpu_file();
    let fence_path = path.with_extension("fence");
    let fence = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&fence_path)
        .unwrap();
    fence.lock_shared().unwrap();
    let endpoint = Arc::new(CpuLeaseHandoff::new_bundle(primary, vec![fence.into()]).unwrap());
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "handoff::tests::cpu_bundle_preserves_actual_fence_locks_after_parent_copies_close",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    endpoint.configure(&mut command);
    let mut child = command.spawn().unwrap();
    drop(command);
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        use std::io::BufRead;
        assert_ne!(output.read_line(&mut line).unwrap(), 0, "child closed without its bundle");
        if line.contains("BUNDLE_READY") {
            break;
        }
        line.clear();
    }
    drop(endpoint);
    let contender = std::fs::OpenOptions::new().read(true).write(true).open(&fence_path).unwrap();
    assert!(matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    contender.try_lock().unwrap();
    contender.unlock().unwrap();
    drop(contender);
    std::fs::remove_file(fence_path).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cpu_bundle_count_and_role_are_bound_before_connect() {
    let value = Advertisement {
        owner: std::process::id(),
        birth: 1,
        pair: [129, 0],
        capability: [7; 32],
        path: "/not/a/socket".into(),
    }
    .encode_role(Role::CpuBundle);
    assert!(Advertisement::parse_role(&value, Role::CpuBundle).is_err());
    let value = Advertisement {
        owner: std::process::id(),
        birth: 1,
        pair: [1, 0],
        capability: [7; 32],
        path: "/not/a/socket".into(),
    }
    .encode_role(Role::CpuBundle);
    assert!(Advertisement::parse(&value).is_err());
    assert!(recover_cpu_lease(&value).is_err());
}

#[test]
fn cpu_bundle_transfers_the_exact_minimum_and_maximum_population() {
    let (path, primary) = cpu_file();
    let fence_path = path.with_extension("fence");
    let fence: OwnedFd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&fence_path)
        .unwrap()
        .into();
    assert_eq!(MAX_CPU_FILES, 128);
    for count in [1, 128] {
        let mut handles = vec![primary.try_clone().unwrap()];
        handles.extend((1..count).map(|_| fence.try_clone().unwrap()));
        let (left, right) = streams();
        // Real handoffs cross process boundaries. Receiving all128 duplicates
        // in this sender too can exhaust its ordinary descriptor budget before
        // it tests transport. Keep both real process budgets unchanged.
        struct Receiver(std::process::Child);
        impl Drop for Receiver {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Receiver(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "handoff::tests::cpu_bundle_population_receiver_actor",
                    "--nocapture",
                ])
                .env("TJS_CPU_POPULATION_RECEIVER", count.to_string())
                .stdin(std::process::Stdio::from(OwnedFd::from(right)))
                .spawn()
                .unwrap(),
        );
        send_handles(&left, &handles, Instant::now() + TIMEOUT).unwrap();
        assert!(child.0.wait().unwrap().success());
    }
    drop((primary, fence));
    std::fs::remove_file(fence_path).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cpu_bundle_population_receiver_actor() {
    let Ok(count) = std::env::var("TJS_CPU_POPULATION_RECEIVER") else {
        return;
    };
    let count = count.parse::<usize>().unwrap();
    assert!(matches!(count, 1 | 128));
    // SAFETY: this isolated actor receives its actual private socket atstdin.
    // Duplicate it without claiming an environment-selected descriptor number.
    let fd = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3) };
    assert!(fd >= 3);
    let stream = UnixStream::from(unsafe { OwnedFd::from_raw_fd(fd) });
    stream.set_nonblocking(true).unwrap();
    let received =
        receive_handles(&stream, Instant::now() + TIMEOUT, Role::CpuBundle, count).unwrap();
    assert_eq!(received.len(), count);
    for fd in received {
        // SAFETY: this descriptor was just received and remains owned here.
        assert_ne!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
    }
}

pub(super) fn pipe() -> [OwnedFd; 2] {
    let mut fds = [-1; 2];
    // SAFETY: pipe writes exactly two newly owned descriptors on success.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: this successful pipe call returned each descriptor once.
    let pipe = unsafe { [OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])] };
    for fd in &pipe {
        // SAFETY: these newly owned fds stay live across this flags update.
        assert_eq!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
    }
    pipe
}

fn streams() -> (UnixStream, UnixStream) {
    let (left, right) = UnixStream::pair().unwrap();
    left.set_nonblocking(true).unwrap();
    right.set_nonblocking(true).unwrap();
    (left, right)
}

fn rights_control_bytes(count: usize) -> usize {
    // SAFETY: the bounded test populations fit the platform's CMSG argument.
    unsafe { libc::CMSG_SPACE((count * size_of::<libc::c_int>()) as _) as usize }
}

fn try_send_rights(stream: &UnixStream, fds: &[libc::c_int]) -> io::Result<()> {
    let control_bytes = rights_control_bytes(fds.len());
    let mut control = vec![0_usize; control_bytes.div_ceil(size_of::<usize>())];
    let mut tag = [1_u8];
    let mut iovec = libc::iovec { iov_base: tag.as_mut_ptr().cast(), iov_len: 1 };
    // SAFETY: all optional pointers/counts start empty, then valid buffers are set.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    // SAFETY: the aligned control buffer holds this population's complete frame.
    unsafe {
        let bytes = std::mem::size_of_val(fds);
        message.msg_controllen = libc::CMSG_SPACE(bytes as _) as _;
        assert!(message.msg_controllen as usize <= std::mem::size_of_val(control.as_slice()));
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(bytes as _) as _;
        for (index, fd) in fds.iter().enumerate() {
            libc::CMSG_DATA(header).cast::<libc::c_int>().add(index).write_unaligned(*fd);
        }
        match libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) {
            1 => Ok(()),
            -1 => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("rights frame was not sent completely")),
        }
    }
}

fn arbitrary_rights(stream: &UnixStream, fds: &[libc::c_int]) {
    try_send_rights(stream, fds).unwrap();
}

fn no_writer_remains(read: OwnedFd) {
    let mut poll = libc::pollfd { fd: read.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: a single valid pollfd is writable. A leaked delivered write end
    // would keep this unique pipe from reporting EOF.
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 100) }, 1, "rejected fd leaked a writer");
    let mut read = File::from(read);
    assert_eq!(read.read(&mut [0]).unwrap(), 0);
}

#[cfg(feature = "client")]
#[test]
fn transferred_handles_conserve_the_original_pool_and_keep_cloexec() {
    let server = crate::Jobserver::new(1).unwrap();
    let held = server.client().acquire().unwrap();
    let (left, right) = streams();
    send_pipe(&left, server.pipe.as_ref().unwrap(), Instant::now() + TIMEOUT).unwrap();
    let [read, write] = receive_pipe(&right, Instant::now() + TIMEOUT).unwrap();
    assert_eq!(server.client().available().unwrap(), 0);
    for fd in [&read, &write] {
        // SAFETY: inspect only these owned received descriptors.
        assert_ne!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
    }
    drop(held);
    let mut token = [0];
    let mut read = File::from(read);
    assert_eq!(read.read(&mut token).unwrap(), 1);
    assert_eq!(server.client().available().unwrap(), 0, "transfer created a private pool");
    let mut write = File::from(write);
    write.write_all(&token).unwrap();
    assert_eq!(server.client().available().unwrap(), 1, "transfer lost or duplicated a token");
}

// The complete200-rights census needs its own actual descriptor table at
// the inherited limit. Other parallel fixtures keep their default concurrency.
fn isolated_assertions(name: &str, body: impl FnOnce()) {
    const SCOPE: &str = "TRUST_JOBSERVER_ISOLATED_ASSERTIONS";
    const MARKER: &str = "JOBSERVER_ISOLATED_ASSERTIONS_COMPLETE";
    if std::env::var(SCOPE).as_deref() == Ok(name) {
        body();
        println!("{MARKER}");
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(SCOPE, name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated descriptor assertions did not finish in60seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "isolated assertions failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).lines().any(|line| line == MARKER),
        "isolated process did not reach its post-assertion marker"
    );
}

#[test]
fn wrong_count_and_oversized_rights_close_every_received_descriptor() {
    isolated_assertions(
        "handoff::tests::wrong_count_and_oversized_rights_close_every_received_descriptor",
        || {
            wrong_count_and_oversized_rights_body();
        },
    );
}

fn wrong_count_and_oversized_rights_body() {
    for count in [1, 3, 40, 200] {
        let [read, write] = pipe();
        let (left, right) = streams();
        arbitrary_rights(&left, &vec![write.as_raw_fd(); count]);
        let error = receive_pipe(&right, Instant::now() + TIMEOUT).unwrap_err();
        assert!(error.to_string().contains("exactly two untruncated"), "{count}: {error}");
        assert!(error.to_string().contains(&format!("rights={count},")), "{count}: {error}");
        let diagnostic = error.to_string();
        let flags = diagnostic.split_once("flags=0x").unwrap().1.split_once(',').unwrap().0;
        let flags = libc::c_int::from_str_radix(flags, 16).unwrap();
        assert_eq!(flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC), 0, "{count}: {error}");
        // Linux preserves this requested close-on-exec bit in msg_flags.
        #[cfg(target_os = "linux")]
        assert_ne!(flags & libc::MSG_CMSG_CLOEXEC, 0, "{count}: {error}");
        eprintln!("rejected rights count {count}: {error}");
        drop(write);
        // Kernel-held queued rights are owned by the socket, not leaked
        // received descriptors. Close both sockets before checking EOF.
        drop((left, right));
        no_writer_remains(read);
    }
}

#[test]
fn kernel_refuses_the_first_rights_frame_above_receive_capacity() {
    let count = (1..=ANCILLARY_BYTES / size_of::<libc::c_int>() + 1)
        .find(|&count| rights_control_bytes(count) > ANCILLARY_BYTES)
        .unwrap();
    assert!(rights_control_bytes(count - 1) <= ANCILLARY_BYTES);
    assert!(rights_control_bytes(count) > ANCILLARY_BYTES);

    let [read, write] = pipe();
    let (left, right) = streams();
    let result = try_send_rights(&left, &vec![write.as_raw_fd(); count]);
    // Never recvmsg this oversized frame: even unexpected send admission cannot
    // install undisclosed descriptors in this process. Socket closure drops any
    // queued kernel rights before the unchanged unique-pipe EOF witness.
    drop((left, right, write));
    no_writer_remains(read);
    let error = result.expect_err("kernel admitted rights exceeding receive capacity");
    assert!(
        matches!(error.raw_os_error(), Some(libc::EINVAL | libc::EMSGSIZE)),
        "unexpected rejection for {count} rights: {error}"
    );
}

#[test]
fn wrong_pipe_order_refuses_and_closes_all_received_writers() {
    let [read, write] = pipe();
    let (left, right) = streams();
    arbitrary_rights(&left, &[write.as_raw_fd(), write.as_raw_fd()]);
    let error = receive_pipe(&right, Instant::now() + TIMEOUT).unwrap_err();
    assert!(error.to_string().contains("ordered pipe pair"));
    drop(write);
    no_writer_remains(read);
}

#[test]
fn regular_files_and_devices_are_not_scheduler_endpoints() {
    let path = std::env::temp_dir().join(format!("tjs-file-{}", hex(&random().unwrap()[..8])));
    let regular =
        std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
    assert!(regular.metadata().unwrap().is_file());
    std::fs::remove_file(path).unwrap();
    let regular = [OwnedFd::from(regular.try_clone().unwrap()), OwnedFd::from(regular)];
    let devices = [
        OwnedFd::from(File::open("/dev/null").unwrap()),
        OwnedFd::from(File::open("/dev/null").unwrap()),
    ];
    for files in [regular, devices] {
        let (left, right) = streams();
        send_pipe(&left, &files, Instant::now() + TIMEOUT).unwrap();
        assert!(
            receive_pipe(&right, Instant::now() + TIMEOUT)
                .unwrap_err()
                .to_string()
                .contains("ordered pipe pair")
        );
    }
}

#[test]
fn readable_writable_fifo_handles_remain_compatible() {
    let path = std::env::temp_dir().join(format!("tjs-fifo-{}", hex(&random().unwrap()[..8])));
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: this fresh pathname is NUL-terminated and owned by this test.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let first = OwnedFd::from(file);
    let pair = [first.try_clone().unwrap(), first];
    let (left, right) = streams();
    send_pipe(&left, &pair, Instant::now() + TIMEOUT).unwrap();
    let [read, write] = receive_pipe(&right, Instant::now() + TIMEOUT).unwrap();
    File::from(write).write_all(b"+").unwrap();
    let mut byte = [0];
    assert_eq!(File::from(read).read(&mut byte).unwrap(), 1);
    assert_eq!(byte, *b"+");
}

#[test]
fn partial_progress_does_not_renew_the_frame_deadline() {
    let (mut reader, mut writer) = streams();
    writer.write_all(b"xy").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        for _ in 0..600 {
            if stopping.load(Ordering::Acquire) {
                break;
            }
            if writer.write(b"z").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut bytes = [0; 512];
    let error = read_frame(&mut reader, &mut bytes, Instant::now() + Duration::from_millis(100))
        .unwrap_err();
    stop.store(true, Ordering::Release);
    thread.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(&bytes[..2], b"xy", "no actual partial frame was consumed");
}

#[test]
fn endpoint_drop_removes_its_socket_with_an_incomplete_client() {
    let endpoint = Endpoint::new(Arc::new(pipe()), &std::env::temp_dir()).unwrap();
    let advertisement = Advertisement::parse(&endpoint.advertisement).unwrap();
    let mut client = connect(&advertisement.path, Instant::now() + TIMEOUT).unwrap();
    write_frame(&mut client, b"T", Instant::now() + TIMEOUT).unwrap();
    drop(endpoint);
    assert!(!advertisement.path.exists());
    assert!(!advertisement.path.parent().unwrap().exists());
}

#[test]
fn stalled_actual_service_request_expires_without_a_complete_capability() {
    let (mut client, server) = streams();
    let root = Process::current().unwrap();
    let pipe = pipe();
    let pair = [pipe[0].as_raw_fd(), pipe[1].as_raw_fd()];
    let thread = std::thread::spawn(move || serve(server, &root, &pipe, &[9; 32], pair));
    write_frame(&mut client, b"T", Instant::now() + TIMEOUT).unwrap();
    // This invokes the actual service handler, rather than assuming that an
    // endpoint's accept thread has entered it before Drop wins the race.
    assert_eq!(thread.join().unwrap().unwrap_err().kind(), io::ErrorKind::TimedOut);
}

#[test]
fn substituted_directory_cannot_redirect_socket_cleanup() {
    let endpoint = Endpoint::new(Arc::new(pipe()), &std::env::temp_dir()).unwrap();
    let original = endpoint._directory.path.clone();
    let saved = original.with_extension("saved");
    let substitute = original.with_extension("other");
    std::fs::rename(&original, &saved).unwrap();
    std::fs::create_dir(&substitute).unwrap();
    std::fs::write(substitute.join("s"), b"sentinel").unwrap();
    std::os::unix::fs::symlink(&substitute, &original).unwrap();
    drop(endpoint);
    assert_eq!(std::fs::read(substitute.join("s")).unwrap(), b"sentinel");
    assert!(!saved.join("s").exists(), "owned directory socket was left behind");
    std::fs::remove_file(&original).unwrap();
    std::fs::remove_file(substitute.join("s")).unwrap();
    std::fs::remove_dir(substitute).unwrap();
    std::fs::remove_dir(saved).unwrap();
}

#[test]
fn advertisement_roundtrip_and_bounds_are_strict() {
    let advertisement = Advertisement {
        owner: std::process::id(),
        birth: 1,
        pair: [3, 4],
        capability: [7; 32],
        path: PathBuf::from("/tmp/tjs:s"),
    };
    let encoded = advertisement.encode();
    assert_eq!(Advertisement::parse(&encoded).unwrap().encode(), encoded);
    let capability = hex(&[7; 32]);
    for invalid in [
        String::new(),
        format!("v0:2:1:3,4:{capability}:/tmp/s"),
        format!("v1:0:1:3,4:{capability}:/tmp/s"),
        format!("v1:2:0:3,4:{capability}:/tmp/s"),
        format!("v1:2:1:2,4:{capability}:/tmp/s"),
        format!("v1:2:1:3,3:{capability}:/tmp/s"),
        format!("v1:2:1:3,4:{capability}:relative"),
        "v1:2:1:3,4:00:/tmp/s".to_owned(),
    ] {
        assert!(Advertisement::parse(OsStr::new(&invalid)).is_err(), "{invalid}");
    }
}

#[test]
fn recovery_refuses_live_or_partly_closed_slots_before_connecting() {
    for close in [0, 1, 2] {
        let [read, write] = pipe();
        let pair = [read.as_raw_fd(), write.as_raw_fd()];
        let advertisement = Advertisement {
            owner: std::process::id(),
            birth: 1,
            pair,
            capability: [7; 32],
            path: PathBuf::from("/no/such/jobserver/socket"),
        };
        let read = if close == 1 {
            drop(read);
            None
        } else {
            Some(read)
        };
        let write = if close == 2 {
            drop(write);
            None
        } else {
            Some(write)
        };
        let error = recover(&advertisement.encode(), OsStr::new(&flags(pair))).unwrap_err();
        assert!(error.to_string().contains("two closed"), "{error}");
        drop((read, write));
    }
}

#[cfg(feature = "client")]
#[test]
fn failed_endpoint_creation_preserves_the_valid_pool_and_has_no_advertisement() {
    let mut server = crate::Jobserver::new(1).unwrap();
    let held = server.client().acquire().unwrap();
    let mut before = std::process::Command::new("__metadata_only__");
    server.configure(&mut before);
    let before = before
        .get_envs()
        .find_map(|(key, value)| (key == "CARGO_MAKEFLAGS").then_some(value).flatten())
        .unwrap()
        .to_owned();
    let absent = std::env::temp_dir().join(format!("tjs-absent-{}", hex(&random().unwrap()[..8])));
    assert!(!absent.exists());
    let error = server.enable_descendant_handoff_in(&absent).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(server.client().available().unwrap(), 0);
    let mut after = std::process::Command::new("__metadata_only__");
    server.configure(&mut after);
    assert_eq!(
        after
            .get_envs()
            .find_map(|(key, value)| (key == "CARGO_MAKEFLAGS").then_some(value).flatten())
            .unwrap(),
        before.as_os_str()
    );
    assert!(after.get_envs().any(|(key, value)| key == ENV && value.is_none()));
    assert!(server.handoff.is_none());
    drop(held);
    assert_eq!(server.client().available().unwrap(), 1);
    let (left, right) = streams();
    send_pipe(&left, server.pipe.as_ref().unwrap(), Instant::now() + TIMEOUT).unwrap();
    let [read, write] = receive_pipe(&right, Instant::now() + TIMEOUT).unwrap();
    let mut token = [0];
    assert_eq!(File::from(read).read(&mut token).unwrap(), 1);
    assert_eq!(server.client().available().unwrap(), 0);
    File::from(write).write_all(&token).unwrap();
    assert_eq!(server.client().available().unwrap(), 1);
}

#[test]
fn endpoint_debug_never_discloses_its_capability() {
    let endpoint = Endpoint::new(Arc::new(pipe()), &std::env::temp_dir()).unwrap();
    let advertisement = Advertisement::parse(&endpoint.advertisement).unwrap();
    assert!(!format!("{endpoint:?}").contains(&hex(&advertisement.capability)));
}

// Trust: actual sockets, byte-length selection, private custody and cleanup.
#[test]
fn oversized_temp_paths_keep_private_bounded_endpoints() {
    let base = PathBuf::from("/tmp").join(format!("tjs-path-{}", hex(&random().unwrap()[..8])));
    std::fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
    for component in ["x".repeat(128), "é".repeat(64)] {
        let long = base.join(component);
        std::fs::create_dir(&long).unwrap();
        let endpoint = Endpoint::new(Arc::new(pipe()), &long).unwrap();
        let advertisement = Advertisement::parse(&endpoint.advertisement).unwrap();
        let owned = endpoint._directory.file.metadata().unwrap();
        // SAFETY: geteuid only reads this process's effective user ID.
        assert_eq!(owned.uid(), unsafe { libc::geteuid() });
        assert_eq!(owned.mode() & 0o777, 0o700);
        assert_eq!(
            advertisement.path.parent().unwrap().parent().unwrap(),
            std::path::Path::new("/tmp")
        );
        assert!(advertisement.path.as_os_str().len() < 104);
        let stream = UnixStream::connect(&advertisement.path).unwrap();
        drop(stream);
        drop(endpoint);
        assert!(!advertisement.path.parent().unwrap().exists());
        assert_eq!(std::fs::read_dir(&long).unwrap().count(), 0);
        std::fs::remove_dir(long).unwrap();
    }
    // Short explicit bases still control placement. A missing long base must
    // not get silently repaired by selecting the system temporary directory.
    let endpoint = Endpoint::new(Arc::new(pipe()), &base).unwrap();
    assert_eq!(endpoint._directory.path.parent().unwrap(), base);
    drop(endpoint);
    let missing = base.join("m".repeat(128));
    assert_eq!(
        Endpoint::new(Arc::new(pipe()), &missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(Endpoint::new(Arc::new(pipe()), std::path::Path::new("relative")).is_err());
    std::fs::remove_dir(base).unwrap();
}
