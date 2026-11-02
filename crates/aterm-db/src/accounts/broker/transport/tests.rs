// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::Token;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

/// Leave room for nested socket paths even when the system temporary path is long.
pub(crate) fn socket_dir() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("br-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Real wire I/O for the model projection. The negative control omits only the
/// EOF barrier; every other step still traverses the broker's durable path.
pub(crate) fn exchange(
    broker: &Broker,
    token: &Token,
    action: &str,
    omit_eof: bool,
    run: impl FnOnce(&BoundRequest, &[u8], Instant) -> Option<AdapterReply> + Send,
) -> (io::Result<()>, io::Result<Reply>) {
    exchange_with_gate(broker, token, action, omit_eof, |_| true, run)
}

pub(crate) fn exchange_with_gate(
    broker: &Broker,
    token: &Token,
    action: &str,
    omit_eof: bool,
    permit: impl FnOnce(Instant) -> bool + Send,
    run: impl FnOnce(&BoundRequest, &[u8], Instant) -> Option<AdapterReply> + Send,
) -> (io::Result<()>, io::Result<Reply>) {
    let (mut client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    client.set_nonblocking(true).unwrap();
    let mut header = request_header(
        token.bytes(),
        [81; 16],
        "example-v1",
        b"private request",
        60_000,
    )
    .unwrap();
    match action {
        "Version" => header[0] ^= 1,
        "WrongToken" => header[8..40].fill(0),
        "Oversize" => header[61..65].copy_from_slice(&((MAX_BODY + 1) as u32).to_be_bytes()),
        _ => {}
    }
    let until = if action == "Expired" {
        Instant::now()
    } else {
        deadline()
    };
    let mut frame = Zeroizing::new(Vec::with_capacity(HEADER + 10 + 15 + 1));
    frame.extend_from_slice(header.as_ref());
    frame.extend_from_slice(if action == "WrongAdapter" {
        b"example-v2"
    } else {
        b"example-v1"
    });
    frame.extend_from_slice(b"private request");
    match action {
        "ShortHeader" => frame.truncate(20),
        "ShortBody" => {
            frame.pop();
        }
        "Trailing" => frame.push(1),
        _ => {}
    }
    write_all(&mut client, &frame, deadline()).unwrap();
    if action != "NoEof" {
        client.shutdown(Shutdown::Write).unwrap();
    }
    if action == "LostReply" {
        drop(client);
        let result = serve_with_until(server, broker, until, read_end, |_| true, permit, run);
        return (result, Err(refused()));
    }
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            serve_with_until(
                server,
                broker,
                until,
                |stream, deadline| {
                    if omit_eof {
                        Ok(())
                    } else if action == "NoEof" {
                        assert_eq!(
                            stream.read(&mut [0]).unwrap_err().kind(),
                            io::ErrorKind::WouldBlock
                        );
                        // Expire specifically at the EOF barrier, without a timing race
                        // in the preceding header reads or a wall-clock latency budget.
                        read_end(stream, Instant::now())
                    } else {
                        read_end(stream, deadline)
                    }
                },
                |_| true,
                permit,
                run,
            )
        });
        let reply = read_reply(&mut client, deadline());
        (server.join().unwrap(), reply)
    })
}

pub(crate) fn open_request(dir: &Path, token: &Token) -> CtlStream {
    let mut stream = aterm_uds::connect::until(&dir.join(NAME), deadline()).unwrap();
    let header =
        request_header(token.bytes(), [91; 16], "example-v1", b"question", 60_000).unwrap();
    stream.write_all(header.as_ref()).unwrap();
    stream.write_all(b"example-v1question").unwrap();
    stream
}

#[test]
fn token_delivery_requires_exact_frame_and_scrubs_its_debug_output() {
    for size in [31, 32, 33] {
        let (read, mut write) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        write.write_all(&vec![7; size]).unwrap();
        drop(write);
        let result = receive_token_until(read, deadline());
        if size == 32 {
            let secret = result.unwrap();
            assert_eq!(secret.expose(), &[7; 32]);
            assert_eq!(format!("{secret:?}"), "Secret(32 bytes redacted)");
        } else {
            assert!(result.is_err());
        }
    }
    let (read, _open) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    assert_eq!(
        receive_token_until(read, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn endpoint_owns_only_its_socket_and_refuses_existing_paths() {
    let dir = socket_dir();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let endpoint = Endpoint::bind(dir.path()).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(endpoint.path()).unwrap().mode() & 0o777,
        0o600
    );
    assert!(Endpoint::bind(dir.path()).is_err());
    assert!(endpoint.try_accept().unwrap().is_none());
    let _client = aterm_uds::connect::until(endpoint.path(), deadline()).unwrap();
    assert!(endpoint.try_accept().unwrap().is_some());
    let kept = dir.path().join("kept.sock");
    std::fs::rename(endpoint.path(), &kept).unwrap();
    std::fs::write(endpoint.path(), b"replacement must survive").unwrap();
    assert!(endpoint.try_accept().is_err());
    drop(endpoint);
    assert_eq!(
        std::fs::read(dir.path().join(NAME)).unwrap(),
        b"replacement must survive"
    );
    assert!(Endpoint::bind(dir.path()).is_err());
    assert!(kept.exists());
}

#[test]
fn endpoint_refuses_open_or_symlink_directories() {
    let root = socket_dir();
    let dir = root.path().join("open");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Endpoint::bind(&dir).is_err());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(Endpoint::bind(&link).is_err());
    let endpoint = Endpoint::bind(&dir).unwrap();
    assert!(socket_identity(&dir).is_ok());
    drop(endpoint);
    assert!(!dir.join(NAME).exists());
}

#[test]
fn endpoint_preserves_replacement_symlinks_and_refuses_hard_links() {
    let root = socket_dir();
    private_file::create_private_dir(root.path()).unwrap();
    let dir = root.path().join("symlink");
    private_file::create_private_dir(&dir).unwrap();
    let endpoint = Endpoint::bind(&dir).unwrap();
    let path = endpoint.path().to_owned();
    let kept = dir.join("kept.sock");
    std::fs::rename(&path, &kept).unwrap();
    std::os::unix::fs::symlink(&kept, &path).unwrap();
    assert!(endpoint.try_accept().is_err());
    drop(endpoint);
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(kept.exists());
    let dir = root.path().join("hardlink");
    private_file::create_private_dir(&dir).unwrap();
    let endpoint = Endpoint::bind(&dir).unwrap();
    std::fs::hard_link(endpoint.path(), dir.join("linked.sock")).unwrap();
    assert!(endpoint.try_accept().is_err());
    drop(endpoint);
    assert!(dir.join(NAME).exists());
}

#[test]
fn peer_uid_is_read_from_the_socket_and_other_users_are_refused() {
    let (stream, _peer) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    assert!(same_user(&stream).is_ok());
    assert!(same_user_as(&stream, peer::our_uid().wrapping_add(1)).is_err());
    #[cfg(target_vendor = "apple")]
    {
        let lone = aterm_uds::kept_out(std::os::unix::net::UnixDatagram::unbound).unwrap();
        let lone = CtlStream::from(std::os::fd::OwnedFd::from(lone));
        assert!(same_user(&lone).is_err());
    }
}

#[test]
fn frame_bounds_deadlines_and_response_checks() {
    for (logical, adapter, size, millis) in [
        ([0; 16], "example-v1", 0, 1),
        ([1; 16], "bad adapter", 0, 1),
        ([1; 16], "example-v1", MAX_BODY + 1, 1),
        ([1; 16], "example-v1", 0, 0),
        ([1; 16], "example-v1", 0, 60_001),
    ] {
        assert!(request_header(&[1; 32], logical, adapter, &vec![0; size], millis).is_err());
    }
    assert!(request_header(&[1; 31], [1; 16], "example-v1", &[], 1).is_err());
    assert!(AdapterReply::new(&vec![0; MAX_BODY + 1], None).is_err());
    let (mut left, mut right) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    left.set_nonblocking(true).unwrap();
    right.set_nonblocking(true).unwrap();
    assert_eq!(
        read_exact(&mut left, &mut [0], Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        write_all(&mut left, b"x", Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    let mut header = *b"ATBRS01\n\x00\x01\0\0\0\0";
    for (field, value) in [(0, 0), (8, 5), (9, 2), (10, 2)] {
        let saved = header[field];
        header[field] = value;
        write_all(&mut right, &header, deadline()).unwrap();
        assert!(read_reply(&mut left, deadline()).is_err());
        header[field] = saved;
    }
    write_all(&mut right, &header, deadline()).unwrap();
    assert!(read_reply(&mut left, deadline()).is_err()); // Refused cannot claim entry.
    for status in [
        Status::Unknown,
        Status::Completed,
        Status::Rejected,
        Status::Unconfirmed,
    ] {
        header[8] = status as u8;
        header[9] = 0;
        header[13] = u8::from(status != Status::Unconfirmed);
        write_all(&mut right, &header, deadline()).unwrap();
        assert!(
            read_reply(&mut left, deadline()).is_err(),
            "inconsistent {status:?}"
        );
    }
}
