// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Temporary, peer-bound transfer of explicitly named immutable lab artifacts.

use crate::{Fail, Res};
use aterm_json::json;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const USAGE: &str = "artifact-serve --bind LAN_IP:PORT --peer IP --status NEW_ABSOLUTE_PATH --file NAME=ABSOLUTE_PATH [--file ...] [--lifetime-secs 900] [--max-requests 32]";
const MAX_FILES: usize = 16;
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
/// Payload bytes reserved across all responses, including failed partial transfers.
const MAX_SENT: u64 = 8 * 1024 * 1024 * 1024;
const MAX_HEADERS: usize = 8192;
const MAX_CONNECTION: Duration = Duration::from_secs(60);
#[cfg(not(test))]
const IO_WAIT: Duration = Duration::from_secs(5);
/// A unit test's client is its own and reads every byte it asked for, so each read and write
/// waits on the hang detector.
#[cfg(test)]
const IO_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

struct Config {
    bind: SocketAddr,
    peer: IpAddr,
    status: PathBuf,
    files: Vec<(String, PathBuf)>,
    lifetime: Duration,
    requests: u32,
}

fn named(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn config(args: &[String]) -> Result<Config, String> {
    let mut bind = None;
    let mut peer = None;
    let mut status = None;
    let mut files = Vec::new();
    let mut lifetime = None;
    let mut requests = None;
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        let value = &pair[1];
        let duplicate = match pair[0].as_str() {
            "--bind" => bind
                .replace(
                    value
                        .parse::<SocketAddr>()
                        .map_err(|_| "invalid bind address")?,
                )
                .is_some(),
            "--peer" => peer
                .replace(
                    value
                        .parse::<IpAddr>()
                        .map_err(|_| "invalid peer address")?,
                )
                .is_some(),
            "--status" => status.replace(PathBuf::from(value)).is_some(),
            "--lifetime-secs" => lifetime
                .replace(value.parse::<u64>().map_err(|_| "invalid lifetime")?)
                .is_some(),
            "--max-requests" => requests
                .replace(value.parse::<u32>().map_err(|_| "invalid request bound")?)
                .is_some(),
            "--file" => {
                let (name, path) = value
                    .split_once('=')
                    .ok_or("file needs NAME=ABSOLUTE_PATH")?;
                if !named(name)
                    || !Path::new(path).is_absolute()
                    || files.iter().any(|(old, _)| old == name)
                {
                    return Err("invalid or duplicate artifact name/path".into());
                }
                files.push((name.to_owned(), PathBuf::from(path)));
                false
            }
            _ => return Err("unknown artifact-serve option".into()),
        };
        if duplicate {
            return Err("duplicate artifact-serve option".into());
        }
    }
    if !remainder.is_empty() {
        return Err("artifact-serve option has no value".into());
    }
    let bind = bind.ok_or("--bind is required")?;
    let peer = peer.ok_or("--peer is required")?;
    let status = status.ok_or("--status is required")?;
    let lifetime = lifetime.unwrap_or(900);
    let requests = requests.unwrap_or(32);
    if !lan(bind.ip())
        || bind.ip().is_unspecified()
        || bind.ip().is_multicast()
        || peer.is_unspecified()
        || peer.is_multicast()
        || peer == IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
        || !status.is_absolute()
        || !(1..=3600).contains(&lifetime)
        || !(1..=128).contains(&requests)
        || files.is_empty()
        || files.len() > MAX_FILES
    {
        return Err("artifact-serve address, file count or lifetime/request bound refused".into());
    }
    Ok(Config {
        bind,
        peer,
        status,
        files,
        lifetime: Duration::from_secs(lifetime),
        requests,
    })
}

struct Artifact {
    name: String,
    file: File,
    len: u64,
    sha256: String,
}

fn stamp(m: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "artifact transfer deadline")
}

fn snapshot(
    name: &str,
    from: &Path,
    into: &Path,
    deadline: Instant,
    limit: u64,
) -> io::Result<Artifact> {
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(from)?;
    let before = source.metadata()?;
    if !before.is_file() || before.len() > MAX_FILE.min(limit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact is not a bounded regular file",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(into)?;
    let mut sha = aterm_digest::Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        copied += n as u64;
        if copied > before.len() {
            return Err(io::Error::other("artifact changed while snapshotting"));
        }
        file.write_all(&buffer[..n])?;
        sha.update(&buffer[..n]);
    }
    if copied != before.len() || stamp(&before) != stamp(&source.metadata()?) {
        return Err(io::Error::other("artifact changed while snapshotting"));
    }
    file.flush()?;
    file.set_permissions(std::fs::Permissions::from_mode(0o400))?;
    Ok(Artifact {
        name: name.into(),
        file,
        len: copied,
        sha256: aterm_codec::hex::encode_lower(&sha.finalize()),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    Forbidden,
    Missing,
    Malformed,
    TooLarge,
    Budget,
}

impl Refused {
    fn code(self) -> u16 {
        match self {
            Self::Forbidden => 403,
            Self::Missing => 404,
            Self::Malformed => 400,
            Self::TooLarge => 431,
            Self::Budget => 413,
        }
    }
}

fn read_head(reader: &mut impl Read) -> Result<Vec<u8>, Refused> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while head.len() < MAX_HEADERS {
        if reader.read(&mut byte).map_err(|_| Refused::Malformed)? != 1 {
            return Err(Refused::Malformed);
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head);
        }
    }
    Err(Refused::TooLarge)
}

fn select(
    peer: IpAddr,
    expected: IpAddr,
    head: &[u8],
    token: &str,
    artifacts: &[Artifact],
) -> Result<usize, Refused> {
    if peer != expected {
        return Err(Refused::Forbidden);
    }
    if head.len() > MAX_HEADERS || !head.ends_with(b"\r\n\r\n") {
        return Err(Refused::Malformed);
    }
    let text = std::str::from_utf8(head).map_err(|_| Refused::Malformed)?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let request: Vec<_> = lines.next().ok_or(Refused::Malformed)?.split(' ').collect();
    if request.len() != 3 || request[0] != "GET" || !matches!(request[2], "HTTP/1.1" | "HTTP/1.0") {
        return Err(Refused::Malformed);
    }
    let mut hosts = 0;
    for (n, line) in lines.enumerate() {
        if n >= 64 || line.starts_with([' ', '\t']) {
            return Err(Refused::Malformed);
        }
        let (name, value) = line.split_once(':').ok_or(Refused::Malformed)?;
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err(Refused::Malformed);
        }
        if name.eq_ignore_ascii_case("host") {
            hosts += 1;
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
        {
            return Err(Refused::Malformed);
        }
    }
    if hosts > 1 || (request[2] == "HTTP/1.1" && hosts != 1) {
        return Err(Refused::Malformed);
    }
    let (capability, name) = request[1]
        .strip_prefix('/')
        .and_then(|path| path.split_once('/'))
        .ok_or(Refused::Forbidden)?;
    if !aterm_digest::ct_eq(capability.as_bytes(), token.as_bytes()) {
        return Err(Refused::Forbidden);
    }
    if !named(name) {
        return Err(Refused::Missing);
    }
    artifacts
        .iter()
        .position(|file| file.name == name)
        .ok_or(Refused::Missing)
}

fn set_wait(stream: &TcpStream, deadline: Instant) -> io::Result<()> {
    let left = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(timed_out)?;
    let wait = left.min(IO_WAIT);
    if wait.is_zero() {
        return Err(timed_out());
    }
    stream.set_read_timeout(Some(wait))?;
    stream.set_write_timeout(Some(wait))
}

struct DeadlineReader<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}
impl Read for DeadlineReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        set_wait(self.stream, self.deadline)?;
        self.stream.read(bytes)
    }
}

fn send(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        set_wait(stream, deadline)?;
        let n = stream.write(bytes)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "artifact socket closed",
            ));
        }
        bytes = &bytes[n..];
    }
    Ok(())
}

fn serve_one(
    mut stream: TcpStream,
    peer: IpAddr,
    token: &str,
    artifacts: &[Artifact],
    deadline: Instant,
    budget: &mut u64,
) -> io::Result<()> {
    let deadline = deadline.min(Instant::now() + MAX_CONNECTION);
    // Darwin accepts inherit the listener's nonblocking mode. The bounded
    // reader and writer below use socket timeouts, so normalize every stream.
    stream.set_nonblocking(false)?;
    set_wait(&stream, deadline)?;
    let selected = if stream.peer_addr()?.ip() == peer {
        read_head(&mut DeadlineReader {
            stream: &mut stream,
            deadline,
        })
        .and_then(|head| select(peer, peer, &head, token, artifacts))
    } else {
        Err(Refused::Forbidden)
    };
    let file = match selected {
        Ok(index) if artifacts[index].len <= *budget => &artifacts[index],
        Ok(_) => {
            send(
                &mut stream,
                format!(
                    "HTTP/1.1 {} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    Refused::Budget.code()
                )
                .as_bytes(),
                deadline,
            )?;
            return Ok(());
        }
        Err(why) => {
            send(
                &mut stream,
                format!(
                    "HTTP/1.1 {} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    why.code()
                )
                .as_bytes(),
                deadline,
            )?;
            return Ok(());
        }
    };
    *budget -= file.len;
    send(&mut stream, format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", file.len).as_bytes(), deadline)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0;
    while offset < file.len {
        set_wait(&stream, deadline)?;
        let want = usize::try_from((file.len - offset).min(buffer.len() as u64))
            .map_err(io::Error::other)?;
        let n = file.file.read_at(&mut buffer[..want], offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "snapshot truncated",
            ));
        }
        send(&mut stream, &buffer[..n], deadline)?;
        offset += n as u64;
    }
    stream.flush()
}

fn status(
    path: &Path,
    address: SocketAddr,
    token: &str,
    files: &[Artifact],
    lifetime: Duration,
) -> io::Result<()> {
    let base = format!("http://{address}/{token}");
    let files: Vec<_> = files
        .iter()
        .map(|file| {
            json!({
                "name": file.name, "size": file.len, "sha256": file.sha256,
                "url": format!("{base}/{}", file.name)
            })
        })
        .collect();
    let bytes = aterm_json::to_vec(&json!({"schema":1,"pid":std::process::id(),
        "expires_in_seconds":lifetime.as_secs(),"base_url":base,"files":files}))
    .map_err(io::Error::other)?;
    let (temporary, mut file) = aterm_uds::atomic_file::create_temp_beside(path, 0o600)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        std::fs::hard_link(&temporary, path)
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

fn check_stop() -> Result<(), Fail> {
    #[cfg(target_os = "macos")]
    crate::stop::check()?;
    Ok(())
}

fn pause() -> Result<(), Fail> {
    #[cfg(target_os = "macos")]
    crate::stop::sleep(Duration::from_millis(10))?;
    #[cfg(not(target_os = "macos"))]
    std::thread::sleep(Duration::from_millis(10));
    Ok(())
}

/// Serve the explicit file set until its time, request or payload budget is spent.
///
/// # Errors
/// Invalid options, an unreadable or changing input, refused socket/status creation,
/// or an interrupted run. Request-specific refusals close only that connection.
/// Transfer I/O failures are reported and make the completed run exit with 1.
pub fn cmd(args: &[String]) -> Res {
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    let config = config(args).map_err(Fail::usage)?;
    if std::fs::symlink_metadata(&config.status).is_ok() {
        return Err(Fail::die("status path already exists"));
    }
    let deadline = Instant::now() + config.lifetime;
    let snapshots = aterm_tempfile::TempDir::new().map_err(|e| Fail::die(e.to_string()))?;
    let mut files = Vec::new();
    let mut total = 0_u64;
    for (i, (name, path)) in config.files.iter().enumerate() {
        let file = snapshot(
            name,
            path,
            &snapshots.path().join(i.to_string()),
            deadline,
            MAX_TOTAL - total,
        )
        .map_err(|e| Fail::die(format!("artifact snapshot: {e}")))?;
        total = total
            .checked_add(file.len)
            .ok_or_else(|| Fail::die("artifact size overflow"))?;
        if total > MAX_TOTAL {
            return Err(Fail::die("artifact total size exceeds bound"));
        }
        files.push(file);
    }
    let listener = TcpListener::bind(config.bind).map_err(|e| Fail::die(e.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Fail::die(e.to_string()))?;
    let address = listener
        .local_addr()
        .map_err(|e| Fail::die(e.to_string()))?;
    let token = aterm_uds::rand::hex_token::<32>().map_err(|e| Fail::die(e.to_string()))?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| Fail::die("artifact snapshot exceeded lifetime"))?;
    status(&config.status, address, &token, &files, remaining)
        .map_err(|e| Fail::die(format!("private status: {e}")))?;
    println!(
        "artifact-serve ready address={address} files={}",
        files.len()
    );
    let mut requests = 0;
    let mut failed_requests = 0;
    let mut budget = MAX_SENT;
    while requests < config.requests && budget != 0 && Instant::now() < deadline {
        check_stop()?;
        match listener.accept() {
            Ok((stream, _)) => {
                requests += 1;
                if let Err(error) =
                    serve_one(stream, config.peer, &token, &files, deadline, &mut budget)
                {
                    failed_requests += 1;
                    eprintln!("artifact-serve request failed: {error}");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => pause()?,
            Err(e) => return Err(Fail::die(format!("artifact accept: {e}"))),
        }
    }
    println!(
        "artifact-serve done requests={requests} failed_requests={failed_requests} reserved_payload_bytes={}",
        MAX_SENT - budget
    );
    Ok(i32::from(failed_requests != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Vec<String> {
        [
            "--bind",
            "127.0.0.1:0",
            "--peer",
            "127.0.0.1",
            "--status",
            "/tmp/status",
            "--file",
            "one=/tmp/one",
        ]
        .into_iter()
        .chain(extra.iter().copied())
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn configuration_has_explicit_peers_names_and_hard_bounds() {
        assert!(config(&args(&[])).is_ok());
        for bad in [
            vec!["--unknown", "x"],
            vec!["--file", "one=/tmp/two"],
            vec!["--file", "../bad=/tmp/two"],
            vec!["--max-requests", "129"],
            vec!["--lifetime-secs", "3601"],
        ] {
            assert!(config(&args(&bad)).is_err());
        }
        for (flag, value) in [
            ("--bind", "0.0.0.0:0"),
            ("--bind", "198.51.100.1:0"),
            ("--peer", "0.0.0.0"),
            ("--peer", "224.0.0.1"),
        ] {
            let mut argv = args(&[]);
            let index = argv.iter().position(|word| word == flag).unwrap();
            argv[index + 1] = value.into();
            assert!(config(&argv).is_err());
        }
    }

    #[test]
    fn authority_and_routes_are_exact() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let input = tmp.path().join("input");
        std::fs::write(&input, b"allowed bytes").unwrap();
        let file = snapshot(
            "one",
            &input,
            &tmp.path().join("snap"),
            Instant::now() + Duration::from_secs(60),
            MAX_FILE,
        )
        .unwrap();
        let files = [file];
        let peer = "127.0.0.1".parse().unwrap();
        let request =
            |path: &str| format!("GET {path} HTTP/1.1\r\nHost: local\r\n\r\n").into_bytes();
        assert_eq!(
            select(peer, peer, &request("/secret/one"), "secret", &files),
            Ok(0)
        );
        assert_eq!(
            select(
                "127.0.0.2".parse().unwrap(),
                peer,
                &request("/secret/one"),
                "secret",
                &files
            ),
            Err(Refused::Forbidden)
        );
        assert_eq!(
            select(peer, peer, &request("/wrong/one"), "secret", &files),
            Err(Refused::Forbidden)
        );
        for path in [
            "/secret/../one",
            "/secret/%2e%2e",
            "/secret/one?query",
            "/secret/",
            "/secret/missing",
        ] {
            assert_eq!(
                select(peer, peer, &request(path), "secret", &files),
                Err(Refused::Missing)
            );
        }
        assert_eq!(
            read_head(&mut io::Cursor::new(vec![b'a'; MAX_HEADERS + 1])),
            Err(Refused::TooLarge)
        );
    }

    #[test]
    fn transfer_keeps_large_immutable_snapshot_complete_on_a_nonblocking_accept() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let input = tmp.path().join("input");
        let expected: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&input, &expected).unwrap();
        let file = snapshot(
            "one",
            &input,
            &tmp.path().join("snapshot"),
            Instant::now() + Duration::from_secs(60),
            MAX_FILE,
        )
        .unwrap();
        assert_eq!(
            file.sha256,
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&expected))
        );
        std::fs::write(&input, b"changed after snapshot").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Darwin inherits the listener's mode. Exercise it on every host.
            stream.set_nonblocking(true).unwrap();
            let mut budget = MAX_SENT;
            serve_one(
                stream,
                address.ip(),
                "secret",
                &[file],
                Instant::now() + Duration::from_secs(60),
                &mut budget,
            )
            .unwrap();
        });
        client
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        client
            .write_all(b"GET /secret/one HTTP/1.1\r\nHost: local\r\n\r\n")
            .unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).unwrap();
        server.join().unwrap();
        let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(String::from_utf8_lossy(&bytes[..end]).contains("Content-Length: 8388608\r\n"));
        assert_eq!(&bytes[end..], expected);
    }

    #[test]
    fn snapshots_refuse_links_and_directories_and_status_never_overwrites() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let input = tmp.path().join("input");
        std::fs::write(&input, b"one").unwrap();
        let linked = tmp.path().join("link");
        std::os::unix::fs::symlink(&input, &linked).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        assert!(snapshot("one", &linked, &tmp.path().join("no"), deadline, MAX_FILE).is_err());
        assert!(
            snapshot(
                "one",
                tmp.path(),
                &tmp.path().join("no"),
                deadline,
                MAX_FILE
            )
            .is_err()
        );
        assert!(snapshot("one", &input, &tmp.path().join("no"), deadline, 2).is_err());
        let status_path = tmp.path().join("status");
        status(
            &status_path,
            "127.0.0.1:1".parse().unwrap(),
            "secret",
            &[],
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&status_path).unwrap().mode() & 0o777,
            0o600
        );
        let before = std::fs::read(&status_path).unwrap();
        assert!(before.ends_with(b"\n"));
        assert!(aterm_json::from_slice::<aterm_json::Value>(&before).is_ok());
        assert!(
            status(
                &status_path,
                "127.0.0.1:2".parse().unwrap(),
                "different",
                &[],
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert_eq!(std::fs::read(status_path).unwrap(), before);
    }
}
