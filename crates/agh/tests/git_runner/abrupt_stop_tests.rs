// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Abrupt CLI death while real Git is receiving an incomplete local pack.
//! The fixture never signals an observed descendant: closing its own HTTP
//! connection releases an old orphan even when the lifecycle assertion fails.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_uds::process::{self, Identity, ProcessPresence};
use aterm_uds::spawnfd::{self, Child, Stdio};

use super::host_git;
use crate::common;

const HANG: Duration = Duration::from_secs(60);

struct World(Option<aterm_tempfile::TempDir>);

impl World {
    fn new(label: &str) -> Self {
        Self(Some(common::scratch(label)))
    }
    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
    fn keep(mut self) -> PathBuf {
        self.0.take().unwrap().keep()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(root) = self.0.take()
        {
            eprintln!("failed real Git world retained: {}", root.keep().display());
        }
    }
}

struct Serving {
    stop: AtomicBool,
    release: AtomicBool,
    complete: AtomicBool,
    posted: AtomicBool,
    active: Mutex<Option<TcpStream>>,
}

struct PackServer {
    url: String,
    state: Arc<Serving>,
    thread: Option<JoinHandle<()>>,
}

fn packet(bytes: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", bytes.len() + 4).into_bytes();
    out.extend_from_slice(bytes);
    out
}

impl PackServer {
    fn new(oid: String, pack: Vec<u8>) -> Self {
        assert!(pack.starts_with(b"PACK") && pack.len() > 32 && pack.len() < 1024 * 1024);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!(
            "http://127.0.0.1:{}/fixture.git",
            listener.local_addr().unwrap().port()
        );
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(Serving {
            stop: AtomicBool::new(false),
            release: AtomicBool::new(false),
            complete: AtomicBool::new(false),
            posted: AtomicBool::new(false),
            active: Mutex::new(None),
        });
        let owned = state.clone();
        let thread = std::thread::spawn(move || {
            while !owned.stop.load(Ordering::Acquire) {
                let (mut stream, _) = match aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                ) {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("owned Git listener: {error}"),
                };
                stream.set_read_timeout(Some(HANG)).unwrap();
                stream.set_write_timeout(Some(HANG)).unwrap();
                *owned.active.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(stream.try_clone().unwrap());
                let mut input = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                if input.read_line(&mut request).is_err() {
                    continue;
                }
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if input.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(length < 64 * 1024);
                if input.read_exact(&mut vec![0; length]).is_err() {
                    continue;
                }
                if request.starts_with("GET /fixture.git/info/refs?service=git-upload-pack ") {
                    let mut body = packet(b"# service=git-upload-pack\n");
                    body.extend_from_slice(b"0000");
                    body.extend(packet(format!("{oid} refs/heads/main\0\n").as_bytes()));
                    body.extend_from_slice(b"0000");
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&body);
                } else if request.starts_with("POST /fixture.git/git-upload-pack ") {
                    let nak = packet(b"NAK\n");
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        nak.len() + pack.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&nak);
                    let _ = stream.write_all(&pack[..pack.len() - 1]);
                    let _ = stream.flush();
                    owned.posted.store(true, Ordering::Release);
                    while !owned.release.load(Ordering::Acquire)
                        && !owned.stop.load(Ordering::Acquire)
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if owned.complete.load(Ordering::Acquire) {
                        let _ = stream.write_all(&pack[pack.len() - 1..]);
                        let _ = stream.flush();
                    }
                }
                let _ = stream.shutdown(Shutdown::Both);
                owned
                    .active
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
            }
        });
        Self {
            url,
            state,
            thread: Some(thread),
        }
    }

    fn release(&self, complete: bool) {
        self.state.complete.store(complete, Ordering::Release);
        self.state.release.store(true, Ordering::Release);
    }
}

impl Drop for PackServer {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        if let Some(stream) = self
            .state
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if self.thread.take().unwrap().join().is_err() && !std::thread::panicking() {
            panic!("owned pack server failed");
        }
    }
}

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_status(child: &mut Child) -> std::process::ExitStatus {
    let by = Instant::now() + HANG;
    loop {
        if let Some(status) = child.try_wait().expect("own CLI wait") {
            return status;
        }
        assert!(Instant::now() < by, "own CLI hang detector");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn retired(token: Identity) -> bool {
    process::read_identity(token.pid)
        .is_some_and(|current| current.pid != token.pid || current.birth != token.birth)
        || matches!(
            process::process_presence(token.pid),
            ProcessPresence::Absent | ProcessPresence::Exited
        )
}

fn source_pack(root: &Path) -> (String, Vec<u8>) {
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    host_git(&source, &["init", "--quiet", "--template=", "-b", "main"]);
    // Exceed Git/curl's buffering with a bounded incompressible fixture.
    // Its held final byte leaves a real index-pack waiting for input.
    let mut state = 0x1234_abcd_u64;
    let mut payload = Vec::with_capacity(128 * 1024);
    for _ in 0..128 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        payload.push(state as u8);
    }
    std::fs::write(source.join("payload"), payload).unwrap();
    host_git(&source, &["add", "payload"]);
    host_git(
        &source,
        &["commit", "--quiet", "-m", "owned streaming fixture"],
    );
    let oid = host_git(&source, &["rev-parse", "refs/heads/main"]);
    let mut command = aterm_uds::fixture_git::git();
    command
        .arg("-C")
        .arg(source)
        .args(["pack-objects", "--stdout", "--all"]);
    let output = aterm_uds::deadline::output_until(&mut command, Instant::now() + HANG).unwrap();
    assert!(
        output.status.success(),
        "real pack: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (oid, output.stdout)
}

fn cli(root: &Path, url: &str) -> (Owned, PathBuf, PathBuf) {
    let bin = root.join("bin");
    let repo = root.join("destination.git");
    std::fs::create_dir(&bin).unwrap();
    host_git(
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            repo.to_str().unwrap(),
        ],
    );
    // A test-owned PATH alias to the compiled native fixture, never a copied
    // installed tool. It execs real Git; HTTP/index-pack must actually run.
    std::os::unix::fs::symlink(aterm_tempfile::bin_exe("agh-git-fixture"), bin.join("git"))
        .unwrap();
    let git = aterm_uds::fixture_git::git();
    std::fs::write(
        bin.join("program"),
        format!("{}\n", Path::new(git.get_program()).display()),
    )
    .unwrap();
    std::fs::write(bin.join("repo"), format!("{}\n", repo.display())).unwrap();
    std::fs::write(bin.join("url"), format!("{url}\n")).unwrap();
    let mut command = spawnfd::command(aterm_tempfile::bin_exe("agh"));
    command
        .args(["--anonymous", "repo", "public-refs", "alabsystems/aterm"])
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env(
            "PATH",
            std::env::join_paths([
                bin.clone(),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
            ])
            .unwrap(),
        )
        .stdout(Stdio::from(
            std::fs::File::create(root.join("cli.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(root.join("cli.stderr")).unwrap(),
        ))
        .process_group(0);
    (
        Owned(command.spawn().expect("real Agh CLI")),
        bin.join("pid"),
        repo,
    )
}

#[test]
fn real_git_pack_completes_through_the_unchanged_cli_collector() {
    let root = World::new("real-git-pack-positive");
    let (oid, pack) = source_pack(root.path());
    let server = PackServer::new(oid.clone(), pack);
    server.release(true);
    let (mut cli, _, repo) = cli(root.path(), &server.url);
    let status = wait_status(&mut cli.0);
    assert!(
        status.success(),
        "{status:?}: {}",
        std::fs::read_to_string(root.path().join("cli.stderr")).unwrap()
    );
    assert!(server.state.posted.load(Ordering::Acquire));
    assert_eq!(host_git(&repo, &["rev-parse", "refs/agh/test"]), oid);
}

#[test]
fn abrupt_cli_death_retires_real_git_and_index_pack_without_signalling_a_bystander() {
    let root = World::new("abrupt-real-git-pack");
    let (oid, pack) = source_pack(root.path());
    let server = PackServer::new(oid, pack);
    let (mut cli, marker, _) = cli(root.path(), &server.url);
    let mut bystander = Owned(spawnfd::command("/bin/sleep").arg("600").spawn().unwrap());
    let by = Instant::now() + HANG;
    let mut last_observed = String::new();
    let (root_token, tokens) = loop {
        let status = cli.0.try_wait().unwrap();
        assert!(
            status.is_none(),
            "CLI ended before real pack reception {status:?}: {}",
            std::fs::read_to_string(root.path().join("cli.stderr")).unwrap()
        );
        let observed = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
            .and_then(|pid| process::group_members(pid).map(|group| (pid, group.pids)));
        if let Some((pid, members)) = observed {
            last_observed = format!(
                "posted={} group={pid} members={:?}",
                server.state.posted.load(Ordering::Acquire),
                members
                    .iter()
                    .map(|member| (*member, process::read_process_argv(*member)))
                    .collect::<Vec<_>>()
            );
            let root_identity = process::read_identity(pid as i32);
            let index = members.iter().find_map(|member| {
                let argv = process::read_process_argv(*member)?;
                argv.iter()
                    .any(|arg| {
                        arg == "index-pack"
                            || Path::new(arg)
                                .file_name()
                                .is_some_and(|name| name == "git-index-pack")
                    })
                    .then(|| process::read_identity(*member))
                    .flatten()
            });
            let http = members.iter().find_map(|member| {
                let argv = process::read_process_argv(*member)?;
                argv.iter()
                    .any(|arg| arg.to_string_lossy().contains("remote-http"))
                    .then(|| process::read_identity(*member))
                    .flatten()
            });
            if let (Some(root), Some(index), Some(http)) = (root_identity, index, http)
                && server.state.posted.load(Ordering::Acquire)
                // The tether's watcher holds the group by its anchor, a zombie that reads no
                // identity; every other member must read.
                && let Some(tokens) = members
                    .iter()
                    .filter(|pid| {
                        process::process_presence(**pid) != process::ProcessPresence::Exited
                    })
                    .map(|pid| process::read_identity(*pid))
                    .collect::<Option<Vec<_>>>()
            {
                assert!(
                    tokens.contains(&root) && tokens.contains(&index) && tokens.contains(&http)
                );
                break (root, tokens);
            }
        }
        if Instant::now() >= by {
            std::fs::write(root.path().join("members.txt"), &last_observed).unwrap();
            panic!("real Git/index-pack did not reach held native reception: {last_observed}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_ne!(
        root_token.pid as u32,
        cli.0.id(),
        "the owned Git scope is separate from its CLI"
    );
    cli.0
        .kill()
        .expect("SIGKILL only our actual frontend handle");
    let status = wait_status(&mut cli.0);
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(status.signal(), Some(9));
    let by = Instant::now() + HANG;
    while !tokens.iter().copied().all(retired) && Instant::now() < by {
        std::thread::sleep(Duration::from_millis(10));
    }
    let before_release = tokens.iter().copied().map(retired).collect::<Vec<_>>();
    let kernel_at_return = tokens
        .iter()
        .map(|token| {
            (
                process::read_identity(token.pid),
                process::process_presence(token.pid),
            )
        })
        .collect::<Vec<_>>();
    let git_trace =
        std::fs::read_to_string(root.path().join("bin/git-trace.json")).unwrap_or_default();
    let bystander_alive = bystander.0.try_wait().unwrap().is_none();
    let evidence = format!(
        "actual frontend status: {status:?}\nroot identity: {root_token:?}\nall observed group identities (including HTTP/index-pack): {tokens:?}\nkernel state before release: {kernel_at_return:?}\nretired before HTTP release: {before_release:?}\nbystander alive: {bystander_alive}\nGit trace before HTTP release: {git_trace}\n"
    );
    std::fs::write(root.path().join("observed.txt"), &evidence).unwrap();
    eprintln!("{evidence}");
    // Closing OUR socket lets the baseline Git finish naturally. No observed
    // descendant PID is used as signalling authority, including on failure.
    server.release(false);
    drop(server);
    let by = Instant::now() + HANG;
    while !tokens.iter().copied().all(retired) && Instant::now() < by {
        std::thread::sleep(Duration::from_millis(10));
    }
    let after_release = tokens.iter().copied().map(retired).collect::<Vec<_>>();
    if !before_release.iter().all(|value| *value)
        || !bystander_alive
        || !after_release.iter().all(|value| *value)
    {
        let retained = root.keep();
        eprintln!("failed abrupt Git world retained: {}", retained.display());
    }
    assert!(
        after_release.iter().all(|value| *value),
        "owned network closure released the old fixture"
    );
    assert!(
        bystander_alive,
        "an unrelated owned process was not signalled"
    );
    assert!(
        before_release.iter().all(|value| *value),
        "real Git group including remote-http + index-pack outlived abrupt frontend death"
    );
}
