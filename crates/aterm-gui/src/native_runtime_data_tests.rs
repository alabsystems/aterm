// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Read;
use std::os::unix::net::UnixListener;

const PAYLOAD: &[u8] = b"opaque forest: primary, private child, goal clock, queued input";
const CHILD_SOCKET: &str = "ATERM_RUNTIME_DATA_FIXTURE_SOCKET";

#[test]
fn a_dead_empty_producer_can_be_replaced_but_retained_bytes_cannot() {
    let mut child = aterm_uds::spawnfd::command("/bin/cat")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    let input = aterm_uds::deadline::cat_echoes(&mut child);
    let owner = Process::observe(child.id()).unwrap();
    let operation = "e".repeat(32);
    let slot = Slot::reserved(operation.clone(), owner.clone());
    let current = Process::observe(std::process::id()).unwrap();
    assert!(!slot.reusable_by(&"f".repeat(32), &current));
    let pin = slot.pin(&operation, &owner).unwrap();
    drop(input);
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        let left = by.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "cat must exit");
        exit.wait(left);
    }
    assert!(
        !slot.reusable_by(&"f".repeat(32), &current),
        "native custody still pins the empty slot"
    );
    drop(pin);
    assert!(slot.reusable_by(&"f".repeat(32), &current));
    let (reader, writer) = unlinked(PAYLOAD);
    drop(writer);
    slot.state.lock().unwrap().outcome =
        Outcome::Ready(Arc::new(Snapshot::copy_from(reader).unwrap()));
    assert!(
        !slot.reusable_by(&"f".repeat(32), &current),
        "producer death must never discard retained recovery state"
    );
}

#[test]
fn operation_pin_covers_reservation_copy_retry_and_explicit_release() {
    for retry in [false, true] {
        let handle = crate::session_store::test_handle_on(918036 + u64::from(retry), -1);
        let ctx = &handle.ctx;
        let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
        let _direct = crate::control::DirectPeer::enter();
        let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
        let operation = "d".repeat(32);
        let owner = Process::observe(std::process::id()).unwrap();
        // Data-only fixture producer record. No SDK or execution authority is
        // manufactured by reserving or pinning this unfilled data slot.
        let slot = Slot::reserved(operation.clone(), owner.clone());
        ctx.fenced_writes.native.lock().unwrap().runtime_data = Some(slot.clone());
        let pin = slot.pin(&operation, &owner).unwrap();
        assert!(pin.retained().is_none());
        assert!(slot.pin(&"e".repeat(32), &owner).is_none());
        let mut wrong = owner.clone();
        wrong.birth += 1;
        assert!(slot.pin(&operation, &wrong).is_none());
        let release = format!("release operation={operation}");
        assert!(serve(ctx, -1, &release).contains("state=busy"));
        let directory = aterm_tempfile::Builder::new()
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .unwrap();
        if retry {
            let missing = directory.path().join("missing");
            serve(
                ctx,
                -1,
                &format!(
                    "retain operation={operation} socket={}",
                    aterm_codec::pct::encode(missing.to_str().unwrap())
                ),
            );
            let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !matches!(slot.state.lock().unwrap().outcome, Outcome::Failed) {
                assert!(Instant::now() < by, "failed transport must settle");
                aterm_uds::deadline::poll_yield();
            }
            assert!(pin.retained().is_none());
            assert!(serve(ctx, -1, &release).contains("state=busy"));
        }
        let socket = directory.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        serve(
            ctx,
            -1,
            &format!(
                "retain operation={operation} socket={}",
                aterm_codec::pct::encode(socket.to_str().unwrap())
            ),
        );
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let (reader, writer) = unlinked(PAYLOAD);
        drop(writer);
        assert_eq!(
            aterm_uds::fdpass::send_with_fds(&stream, b"S", &[reader.as_fd()]).unwrap(),
            1
        );
        let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let retained = loop {
            if let Some(retained) = pin.retained() {
                break retained;
            }
            assert!(Instant::now() < by, "host copy must settle");
            aterm_uds::deadline::poll_yield();
        };
        assert_eq!(retained.0, PAYLOAD.len() as u64);
        assert_eq!(
            retained.1,
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(PAYLOAD))
        );
        assert!(serve(ctx, -1, &release).contains("state=busy"));
        drop(pin);
        assert!(serve(ctx, -1, &release).contains("state=released"));
    }
}

fn unlinked(bytes: &[u8]) -> (File, File) {
    let directory = aterm_tempfile::Builder::new()
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let path = directory.path().join("state");
    let mut temporary = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    temporary.write_all(bytes).unwrap();
    let reader = File::open(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    (reader, temporary)
}

#[test]
fn host_copy_is_immutable_even_if_source_keeps_a_writer() {
    let (reader, writer) = unlinked(PAYLOAD);
    let snapshot = Snapshot::copy_from(reader).unwrap();
    writer.write_all_at(b"CHANGED", 0).unwrap();
    let mut copied = vec![0; PAYLOAD.len()];
    snapshot.file.read_exact_at(&mut copied, 0).unwrap();
    assert_eq!(copied, PAYLOAD);
    assert_eq!(
        snapshot.digest,
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(PAYLOAD))
    );
    assert_eq!(snapshot.file.metadata().unwrap().nlink(), 0);
}

#[test]
fn descriptor_admission_refuses_writable_linked_empty_and_oversized_files() {
    let (reader, writer) = unlinked(PAYLOAD);
    assert!(Snapshot::copy_from(writer).is_err());
    drop(reader);
    let directory = aterm_tempfile::Builder::new()
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let path = directory.path().join("linked");
    std::fs::write(&path, b"x").unwrap();
    assert!(Snapshot::copy_from(File::open(path).unwrap()).is_err());
    let (empty, _writer) = unlinked(b"");
    assert!(Snapshot::copy_from(empty).is_err());
    let (oversized, writer) = unlinked(b"x");
    writer.set_len(MAX_BYTES + 1).unwrap();
    assert!(Snapshot::copy_from(oversized).is_err());
}

#[test]
#[ignore = "private subprocess of host_retains_snapshot_after_the_real_producer_exits"]
fn runtime_data_source_child() {
    let socket = std::env::var_os(CHILD_SOCKET).expect("fixture socket");
    let listener = UnixListener::bind(socket).unwrap();
    println!("runtime-data-ready");
    std::io::stdout().flush().unwrap();
    let (stream, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let (reader, writer) = unlinked(PAYLOAD);
    drop(writer);
    assert_eq!(
        aterm_uds::fdpass::send_with_fds(&stream, b"S", &[reader.as_fd()]).unwrap(),
        1
    );
    // The host has authenticated the live peer before asking for the data.
    let mut acknowledged = [0];
    aterm_uds::hangup::read_until(
        &stream,
        &mut acknowledged,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(acknowledged, *b"K");
}

#[test]
fn host_retains_snapshot_after_the_real_producer_exits() {
    let directory = aterm_tempfile::Builder::new()
        .prefix("runtime-data-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = directory.path().join("s");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_act::runtime_data::tests::runtime_data_source_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_SOCKET, &socket)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    aterm_uds::deadline::prints(
        &mut child,
        "runtime-data-ready",
        aterm_spec::wait::MUST_HAPPEN,
    );
    let owner = Process::observe(child.id()).unwrap();
    let stream = connect(&socket, &owner, Instant::now() + TRANSFER_TIME).unwrap();
    aterm_uds::ready::until(
        stream.as_fd(),
        aterm_uds::ready::Interest::Read,
        Instant::now() + TRANSFER_TIME,
    )
    .unwrap();
    let mut marker = [0];
    let mut received = aterm_uds::fdpass::recv_with_fds(&stream, &mut marker, 1).unwrap();
    assert_eq!(marker, *b"S");
    assert_eq!(received.fds.len(), 1);
    let snapshot = Arc::new(Snapshot::copy_from(File::from(received.fds.remove(0))).unwrap());
    (&stream).write_all(b"K").unwrap();
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "producer must exit");
        exit.wait(left);
    }
    // No source process or source description remains. Fetching repeatedly
    // retains the host original, including after another reader moved its offset.
    let current = Process::observe(std::process::id()).unwrap();
    for attempt in 0..2 {
        let fetch = directory.path().join(format!("f{attempt}"));
        let listener = UnixListener::bind(&fetch).unwrap();
        send(&fetch, &current, &snapshot).unwrap();
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let mut received = aterm_uds::fdpass::recv_with_fds(&stream, &mut marker, 1).unwrap();
        let mut reader = File::from(received.fds.remove(0));
        let mut bytes = vec![0; PAYLOAD.len()];
        reader.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, PAYLOAD);
        let mut ignored = Vec::new();
        reader.read_to_end(&mut ignored).unwrap();
    }
    // A side socket owned by this process cannot impersonate the departed
    // producer, even if its pathname was supplied with that PID's metadata.
    let other = directory.path().join("other");
    let _listener = UnixListener::bind(&other).unwrap();
    assert!(
        connect(&other, &owner, Instant::now() + TRANSFER_TIME)
            .unwrap_err()
            .to_string()
            .contains("another owner")
    );
}

#[test]
#[ignore = "private subprocess of control_endpoint_preserves_data_and_rejects_a_foreign_kernel_peer"]
fn runtime_data_foreign_child() {
    let socket = std::env::var_os(CHILD_SOCKET).expect("fixture socket");
    let stream = aterm_uds::CtlStream::connect(socket).unwrap();
    println!("runtime-data-connected");
    std::io::stdout().flush().unwrap();
    let mut reply = [0];
    aterm_uds::hangup::read_until(
        &stream,
        &mut reply,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(reply, *b"K");
}

#[test]
fn control_endpoint_preserves_data_and_rejects_a_foreign_kernel_peer() {
    let handle = crate::session_store::test_handle_on(918034, -1);
    let ctx = &handle.ctx;
    let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
    let _direct = crate::control::DirectPeer::enter();
    let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    let operation = "a".repeat(32);
    // A real kernel peer alone cannot become a producer: no ALab self-claim or
    // foreground PTY exists in this fixture, and retain must allocate nothing.
    assert_eq!(
        serve(
            ctx,
            -1,
            &format!("retain operation={operation} socket=/nonexistent")
        ),
        REFUSED
    );
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .runtime_data
            .is_none()
    );
    let (reader, writer) = unlinked(PAYLOAD);
    drop(writer);
    let snapshot = Arc::new(Snapshot::copy_from(reader).unwrap());
    let mut owner = Process::observe(std::process::id()).unwrap();
    // Data-only producer-record injection. A prior image's record can be read
    // after exec; it supplies no SDK constructor or native execution grant.
    owner.image = Image::at(Path::new("/bin/cat")).unwrap();
    ctx.fenced_writes.native.lock().unwrap().runtime_data = Some(Arc::new(Slot {
        operation: operation.clone(),
        owner,
        state: Mutex::new(State {
            outcome: Outcome::Ready(snapshot),
            sending: false,
            pins: 0,
        }),
    }));
    assert!(serve(ctx, -1, "discover").contains(&format!("operation={operation}")));
    assert_eq!(
        serve(
            ctx,
            -1,
            &format!("retain operation={} socket=/nonexistent", "b".repeat(32))
        ),
        REFUSED
    );
    let directory = aterm_tempfile::Builder::new()
        .prefix("runtime-control-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = directory.path().join("f");
    let listener = UnixListener::bind(&socket).unwrap();
    assert_eq!(
        serve(
            ctx,
            -1,
            &format!(
                "fetch operation={operation} socket={}",
                aterm_codec::pct::encode(socket.to_str().unwrap())
            )
        ),
        "OK native runtime-data state=sending\n"
    );
    let (data, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let mut marker = [0];
    let mut rights = aterm_uds::fdpass::recv_with_fds(&data, &mut marker, 1).unwrap();
    assert_eq!(marker, *b"S");
    let reader = File::from(rights.fds.remove(0));
    let mut bytes = vec![0; PAYLOAD.len()];
    reader.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(bytes, PAYLOAD);
    assert!(serve(ctx, -1, &format!("status operation={operation}")).contains("state=retained"));

    let foreign_socket = directory.path().join("c");
    let listener = UnixListener::bind(&foreign_socket).unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_act::runtime_data::tests::runtime_data_foreign_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_SOCKET, &foreign_socket)
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    aterm_uds::deadline::prints(
        &mut child,
        "runtime-data-connected",
        aterm_spec::wait::MUST_HAPPEN,
    );
    let (foreign, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Until(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    {
        let _serving = aterm_control::hangup::ServingFd::enter(foreign.as_raw_fd());
        assert_eq!(
            serve(ctx, -1, "discover"),
            "OK native runtime-data state=absent\n"
        );
        assert_eq!(
            serve(ctx, -1, &format!("release operation={operation}")),
            REFUSED
        );
    }
    (&foreign).write_all(b"K").unwrap();
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "foreign peer must exit");
        exit.wait(left);
    }
    loop {
        let reply = serve(ctx, -1, &format!("release operation={operation}"));
        if reply == "OK native runtime-data state=released\n" {
            break;
        }
        assert_eq!(reply, "OK native runtime-data state=busy\n");
        assert!(
            Instant::now() < deadline,
            "fetch must finish before release"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(
        serve(ctx, -1, &format!("release operation={operation}")),
        "OK native runtime-data state=released\n"
    );
}
