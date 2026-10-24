// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vz arm over a scripted session: every command on a socket pair whose far end is a
//! scripted agent (the frames V1's agent sends), so the arm's own work is what is held:
//! §10's four outcomes kept apart, stdin, the merge, §8's environment, the three copies
//! (atpkg's vetted extraction refusing an escape), the share's rebind, and the run's end.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aterm_vz_agent::wire::{self, Exec, Exit, Frame, Hello, Stdin};

use super::*;

/// What the scripted session saw.
type Log = Arc<Mutex<Vec<String>>>;

/// One scripted agent's answer to a request.
type Agent = Arc<dyn Fn(Frame, &mut UnixStream, &Log) + Send + Sync>;

struct Pairs {
    agent: Agent,
    log: Log,
    next: u64,
    refuse: Option<String>,
    kept: bool,
    lost: String,
    /// What `wait_stopped` says: how the VM stopped.
    stopped: Result<String, String>,
}

impl Link for Pairs {
    fn connect(&mut self) -> Result<(u64, OwnedFd), String> {
        if let Some(why) = &self.refuse {
            return Err(why.clone());
        }
        let (ours, mut guest) = UnixStream::pair().map_err(|e| e.to_string())?;
        self.next += 1;
        self.log
            .lock()
            .unwrap()
            .push(format!("connect {}", self.next));
        let (agent, log) = (Arc::clone(&self.agent), Arc::clone(&self.log));
        std::thread::spawn(move || {
            if let Ok(Some(frame)) = wire::read_frame(&mut guest) {
                agent(frame, &mut guest, &log);
            }
        });
        Ok((self.next, OwnedFd::from(ours)))
    }

    fn release(&mut self, id: u64) {
        self.log.lock().unwrap().push(format!("release {id}"));
    }

    fn why_lost(&mut self) -> String {
        self.lost.clone()
    }

    fn keep(&mut self) -> Result<(), String> {
        self.kept = true;
        self.log.lock().unwrap().push("keep".into());
        Ok(())
    }

    fn kept(&self) -> bool {
        self.kept
    }

    fn share(&mut self, entries: &[ShareEntry]) -> Result<(), String> {
        self.log.lock().unwrap().push(format!(
            "share {}",
            entries
                .iter()
                .map(|e| format!("{}={}", e.name, e.path.display()))
                .collect::<Vec<_>>()
                .join(",")
        ));
        Ok(())
    }

    fn wait_stopped(&mut self) -> Result<String, String> {
        // The helper says the VM stopped only once its guest took the `Poweroff`, which the
        // scripted agent logs on its own thread: a one-minute hang detector.
        let begun = std::time::Instant::now();
        while !self.log.lock().unwrap().iter().any(|l| l == "poweroff") {
            if begun.elapsed() >= aterm_vz::detect::SPAN {
                self.log.lock().unwrap().push("no poweroff".into());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.log.lock().unwrap().push("wait_stopped".into());
        self.stopped.clone()
    }

    fn detach(self: Box<Self>) {
        self.log.lock().unwrap().push("detach".into());
    }
}

fn say(guest: &mut UnixStream, frame: &Frame) {
    let _ = wire::write_frame(guest, frame);
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The scripted agent: what each first word of an `Exec`'s argv does.
fn agent(files: BTreeMap<String, Vec<u8>>, archive: Vec<u8>, in_dir: PathBuf) -> Agent {
    Arc::new(move |frame, guest, log| {
        let exec: Exec = match frame {
            Frame::Exec(e) => e,
            Frame::Mount(shares) => {
                log.lock().unwrap().push(format!(
                    "mount {}",
                    shares
                        .iter()
                        .map(|s| format!("{}={}", s.name, text(&s.path)))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                say(guest, &Frame::Exit(Exit::Exited(0)));
                return;
            }
            Frame::Poweroff => {
                log.lock().unwrap().push("poweroff".into());
                return;
            }
            other => panic!("not a request: {other:?}"),
        };
        let argv: Vec<String> = exec.argv.iter().map(|a| text(a)).collect();
        log.lock().unwrap().push(format!("exec {}", argv.join(" ")));
        match argv.first().map(String::as_str) {
            Some("cat") if exec.stdin == Stdin::Pipe => {
                let mut got = Vec::new();
                loop {
                    match wire::read_frame(guest) {
                        Ok(Some(Frame::Stdin(b))) => got.extend_from_slice(&b),
                        Ok(Some(Frame::StdinEof)) => break,
                        other => panic!("stdin: {other:?}"),
                    }
                }
                say(guest, &Frame::Out(got));
                say(guest, &Frame::Exit(Exit::Exited(0)));
            }
            Some("cat") => match files.get(&argv[2]) {
                Some(bytes) => {
                    for chunk in bytes.chunks(1000) {
                        say(guest, &Frame::Out(chunk.to_vec()));
                    }
                    say(guest, &Frame::Exit(Exit::Exited(0)));
                }
                None => {
                    say(guest, &Frame::Err(b"cat: no such file\n".to_vec()));
                    say(guest, &Frame::Exit(Exit::Exited(1)));
                }
            },
            Some("tar") => {
                say(guest, &Frame::Out(archive.clone()));
                say(guest, &Frame::Exit(Exit::Exited(0)));
            }
            Some("printenv") => {
                let env: Vec<String> = exec
                    .env
                    .iter()
                    .map(|(k, v)| format!("{}={}", text(k), text(v)))
                    .collect();
                let mut said = env.join("\n");
                said.push_str(&format!(
                    "\ncwd={}\nmerge={}\n",
                    text(&exec.cwd),
                    exec.merge
                ));
                say(guest, &Frame::Out(said.into_bytes()));
                say(guest, &Frame::Exit(Exit::Exited(0)));
            }
            Some("merged") => {
                assert!(exec.merge, "shell_streamed merges in the guest");
                for piece in ["out-1\nerr", "-2\n", "out-3"] {
                    say(guest, &Frame::Out(piece.as_bytes().to_vec()));
                }
                say(guest, &Frame::Exit(Exit::Exited(0)));
            }
            Some("cp") => {
                // The staged tree is in the in share while the guest copies it.
                let name = Path::new(&argv[3]).file_name().unwrap().to_owned();
                let staged = in_dir.join(&name).join("sub/file.txt");
                log.lock().unwrap().push(format!(
                    "staged {}",
                    std::fs::read_to_string(staged).unwrap_or_default()
                ));
                say(guest, &Frame::Exit(Exit::Exited(0)));
            }
            Some("exit3") => {
                say(guest, &Frame::Err(b"three\n".to_vec()));
                say(guest, &Frame::Exit(Exit::Exited(3)));
            }
            Some("killed") => say(guest, &Frame::Exit(Exit::Signaled(9))),
            Some("sudo") => say(guest, &Frame::Exit(Exit::Exited(0))),
            // coreutils' env, run as given: a program it cannot find is its own exit 127.
            Some("env") => {
                say(
                    guest,
                    &Frame::Err(b"env: '/nonexistent': No such file or directory\n".to_vec()),
                );
                say(guest, &Frame::Exit(Exit::Exited(127)));
            }
            Some("/nonexistent") => say(
                guest,
                &Frame::Exit(Exit::NotStarted(
                    "/nonexistent: No such file or directory (os error 2)".into(),
                )),
            ),
            Some("vanish") => {
                say(guest, &Frame::Out(b"half".to_vec()));
            }
            other => panic!("no script for {other:?}"),
        }
    })
}

struct Rig {
    vz: Vz,
    log: Log,
    dir: aterm_tempfile::TempDir,
}

fn hello() -> Hello {
    Hello {
        proto: aterm_vz_agent::wire::PROTO,
        key: [0; aterm_vz_agent::key::LEN],
        agent_sha256: [0xab; 32],
        boot_id: "b00t".into(),
        kernel: "6.8.0-142-generic".into(),
        uptime_ms: 1830,
    }
}

fn rig_with(files: BTreeMap<String, Vec<u8>>, archive: Vec<u8>, refuse: Option<&str>) -> Rig {
    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path().join("vz"));
    let in_dir = paths.root().join(IN);
    std::fs::create_dir_all(&in_dir).unwrap();
    let log = Log::default();
    let link = Pairs {
        agent: agent(files, archive, in_dir),
        log: Arc::clone(&log),
        next: 0,
        refuse: refuse.map(str::to_string),
        kept: false,
        lost: "the guest stopped answering (no heartbeat for 60 s); VM stopped".into(),
        stopped: Ok(aterm_vz::helper::POWERED_OFF.into()),
    };
    let user = GuestUser {
        name: "lane".into(),
        uid: 501,
    };
    Rig {
        vz: Vz::new(Box::new(link), user, paths, "4e".repeat(32), hello()),
        log,
        dir,
    }
}

fn rig() -> Rig {
    rig_with(BTreeMap::new(), Vec::new(), None)
}

/// A rig whose VM stops with `stopped` once the bake powers it off.
fn rig_stopping(stopped: Result<String, String>) -> Rig {
    let base = rig();
    let log = Arc::clone(&base.log);
    let in_dir = base.vz.in_dir.clone();
    let link = Pairs {
        agent: agent(BTreeMap::new(), Vec::new(), in_dir),
        log: Arc::clone(&log),
        next: 0,
        refuse: None,
        kept: false,
        lost: String::new(),
        stopped,
    };
    Rig {
        vz: Vz::new(
            Box::new(link),
            base.vz.user.clone(),
            base.vz.paths.clone(),
            "4e".repeat(32),
            hello(),
        ),
        log,
        dir: base.dir,
    }
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn logged(rig: &Rig) -> Vec<String> {
    rig.log.lock().unwrap().clone()
}

#[test]
fn the_four_outcomes_are_kept_apart_in_out() {
    let rig = rig();
    let exit = rig.vz.shell("/", &args(&["exit3"]), None);
    assert_eq!((exit.code, exit.text.as_str()), (Some(3), "three\n"));
    assert_eq!(exit.status(), "exit 3");
    let signal = rig.vz.shell("/", &args(&["killed"]), None);
    assert_eq!(signal.code, None);
    assert!(signal.text.contains("ended on signal 9"), "{}", signal.text);
    assert!(signal.not_started.is_none() && signal.lost.is_none());
    let missing = rig.vz.shell("/", &args(&["/nonexistent"]), None);
    assert!(!missing.ok());
    assert_eq!(missing.code, None);
    assert_eq!(
        missing.status(),
        "not started: /nonexistent: No such file or directory (os error 2)"
    );
    let lost = rig.vz.shell("/", &args(&["vanish"]), None);
    assert!(!lost.ok());
    assert_eq!(lost.text, "half", "what came before the loss is kept");
    assert_eq!(
        lost.status(),
        "lost: the connection ended before its Exit: the guest stopped answering (no \
         heartbeat for 60 s); VM stopped"
    );
    // Every connection was released, the lost one too.
    let log = logged(&rig);
    for id in 1..=4 {
        assert!(log.contains(&format!("release {id}")), "{log:?}");
    }
}

#[test]
fn a_refused_connection_is_lost_naming_the_refusal() {
    let rig = rig_with(
        BTreeMap::new(),
        Vec::new(),
        Some("the VM is Stopping: no connection"),
    );
    let out = rig.vz.shell("/", &args(&["true"]), None);
    assert_eq!(
        out.status(),
        "lost: no command connection: the VM is Stopping: no connection"
    );
}

#[test]
fn stdin_is_piped_and_every_exec_gets_the_lane_users_environment() {
    let rig = rig();
    let big = "x".repeat(200_000) + "\n";
    let out = rig
        .vz
        .shell("/home/lane.guest", &args(&["cat"]), Some(&big));
    assert!(out.ok(), "{}", out.status());
    assert_eq!(out.text, big, "stdin in several chunks, then its EOF");
    let env = rig.vz.shell("/home/lane.guest", &args(&["printenv"]), None);
    let lines: Vec<&str> = env.text.lines().collect();
    for want in [
        "HOME=/home/lane.guest",
        "USER=lane",
        "LOGNAME=lane",
        "SHELL=/bin/bash",
        "LANG=C.UTF-8",
        "XDG_RUNTIME_DIR=/run/user/501",
        "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/501/bus",
        "cwd=/home/lane.guest",
        "merge=false",
    ] {
        assert!(lines.contains(&want), "{want}: {lines:?}");
    }
    assert!(
        lines.iter().any(|l| l.starts_with("PATH=/usr/local/sbin:")),
        "{lines:?}"
    );
}

#[test]
fn a_streamed_command_merges_in_the_guest_and_its_lines_come_whole() {
    let rig = rig();
    let out = rig.vz.shell_streamed("/", &args(&["merged"]), "  | ");
    assert!(out.ok());
    assert_eq!(out.text, "out-1\nerr-2\nout-3");
    let mut lines = Lines::default();
    let mut seen = Vec::new();
    for piece in ["a\nb", "c\r\n", "", "d"] {
        lines.feed(piece.as_bytes(), &mut |l| seen.push(l.to_string()));
    }
    lines.finish(&mut |l| seen.push(l.to_string()));
    assert_eq!(seen, ["a", "bc", "d"]);
}

#[test]
fn copy_out_writes_the_files_bytes_and_a_failure_writes_nothing() {
    let bytes: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let rig = rig_with(
        BTreeMap::from([("/tmp/frame.png".to_string(), bytes.clone())]),
        Vec::new(),
        None,
    );
    let to = rig.dir.path().join("frame.png");
    let out = rig.vz.copy_out("/tmp/frame.png", &to);
    assert!(out.ok(), "{}", out.status());
    assert_eq!(std::fs::read(&to).unwrap(), bytes);
    assert!(
        logged(&rig).contains(&"exec cat -- /tmp/frame.png".to_string()),
        "{:?}",
        logged(&rig)
    );
    let missing = rig.dir.path().join("missing.png");
    let out = rig.vz.copy_out("/tmp/missing.png", &missing);
    assert_eq!(out.code, Some(1));
    assert!(out.text.contains("no such file"));
    assert!(!missing.exists());
}

/// A `.tar.gz` of `entries` (a directory where the bytes are `None`): ustar headers, and
/// gzip's stored blocks.
fn targz(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, data) in entries {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let octal = |h: &mut [u8; 512], at: usize, width: usize, n: u64| {
            let s = format!("{n:0w$o}\0", w = width - 1);
            h[at..at + width].copy_from_slice(s.as_bytes());
        };
        octal(&mut h, 100, 8, if data.is_some() { 0o644 } else { 0o755 });
        octal(&mut h, 108, 8, 0);
        octal(&mut h, 116, 8, 0);
        octal(&mut h, 124, 12, data.map_or(0, |d| d.len() as u64));
        octal(&mut h, 136, 12, 1_792_000_000);
        h[148..156].copy_from_slice(b"        ");
        h[156] = if data.is_some() { b'0' } else { b'5' };
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        tar.extend_from_slice(&h);
        if let Some(d) = data {
            tar.extend_from_slice(d);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let chunks: Vec<&[u8]> = tar.chunks(65_535).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        gz.push(u8::from(i + 1 == chunks.len()));
        let len = u16::try_from(chunk.len()).unwrap();
        gz.extend_from_slice(&len.to_le_bytes());
        gz.extend_from_slice(&(!len).to_le_bytes());
        gz.extend_from_slice(chunk);
    }
    gz.extend_from_slice(&aterm_codec::crc32::Crc32::of(&tar).to_le_bytes());
    gz.extend_from_slice(&u32::try_from(tar.len()).unwrap().to_le_bytes());
    gz
}

#[test]
fn copy_dir_out_unpacks_the_guests_tar_under_its_own_name_and_refuses_an_escape() {
    let archive = targz(&[
        ("frames/", None),
        ("frames/a/", None),
        ("frames/a/one.txt", Some(b"one\n")),
        ("frames/top.txt", Some(b"top\n")),
    ]);
    let rig = rig_with(BTreeMap::new(), archive, None);
    let parent = rig.dir.path().join("results");
    std::fs::create_dir_all(&parent).unwrap();
    let out = rig
        .vz
        .copy_dir_out("/home/lane.guest/aterm-lane/frames", &parent);
    assert!(out.ok(), "{}: {}", out.status(), out.text);
    assert_eq!(
        std::fs::read_to_string(parent.join("frames/a/one.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(parent.join("frames/top.txt")).unwrap(),
        "top\n"
    );
    assert!(
        logged(&rig)
            .contains(&"exec tar -C /home/lane.guest/aterm-lane -czf - -- frames".to_string()),
        "{:?}",
        logged(&rig)
    );
    // Only the tree is left: the staging went.
    let left: Vec<_> = std::fs::read_dir(&parent)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left, ["frames"]);
    // A second copy over it copies nothing over it.
    let again = rig
        .vz
        .copy_dir_out("/home/lane.guest/aterm-lane/frames", &parent);
    assert_eq!(again.code, Some(1));
    assert!(again.text.contains("exists"), "{}", again.text);
    // The negative control: a member that climbs out of the tree is refused, and nothing
    // lands beside it.
    let evil = targz(&[("frames/", None), ("frames/../../escaped.txt", Some(b"x"))]);
    let rig = rig_with(BTreeMap::new(), evil, None);
    let parent = rig.dir.path().join("results");
    std::fs::create_dir_all(&parent).unwrap();
    let out = rig.vz.copy_dir_out("/g/frames", &parent);
    assert_eq!(out.code, Some(1), "{}", out.text);
    assert!(!rig.dir.path().join("escaped.txt").exists());
    assert!(!parent.join("frames").exists());
}

#[test]
fn copy_dir_in_stages_the_tree_in_the_in_share_then_removes_it() {
    let rig = rig();
    let site = rig.dir.path().join("site");
    std::fs::create_dir_all(site.join("sub")).unwrap();
    std::fs::write(site.join("sub/file.txt"), "from the Mac\n").unwrap();
    let out = rig.vz.copy_dir_in(&site, "/home/lane.guest/aterm-lane/");
    assert!(out.ok(), "{}", out.status());
    let log = logged(&rig);
    assert!(
        log.contains(&"exec cp -a -- /mnt/aterm/in/site /home/lane.guest/aterm-lane/".to_string()),
        "{log:?}"
    );
    assert!(
        log.contains(&"staged from the Mac\n".to_string()),
        "{log:?}"
    );
    assert!(
        !rig.vz.in_dir.join("site").exists(),
        "the staged copy is removed"
    );
    assert!(
        site.join("sub/file.txt").exists(),
        "the source is untouched"
    );
}

#[test]
fn the_share_mounts_without_the_in_entry_and_a_swap_rebinds() {
    let rig = rig();
    let shares = vec![
        ShareEntry {
            name: "repo".into(),
            path: "/src/aterm".into(),
        },
        ShareEntry {
            name: IN.into(),
            path: rig.vz.in_dir.clone(),
        },
    ];
    rig.vz.mount(&shares).unwrap();
    let mut other = shares.clone();
    other[0].path = "/src/other".into();
    rig.vz.swap(&other).unwrap();
    let log: Vec<String> = logged(&rig)
        .into_iter()
        .filter(|l| l.starts_with("mount") || l.starts_with("share"))
        .collect();
    assert_eq!(
        log,
        [
            "mount repo=/src/aterm".to_string(),
            format!("share repo=/src/other,in={}", rig.vz.in_dir.display()),
            "mount repo=/src/other".to_string(),
        ]
    );
}

#[test]
fn the_runs_end_sets_keep_only_when_asked_and_always_closes_the_session() {
    let kept = rig();
    let log = Arc::clone(&kept.log);
    kept.vz.end(true, true);
    assert_eq!(*log.lock().unwrap(), ["keep", "detach"]);
    let released = rig();
    let log = Arc::clone(&released.log);
    released.vz.end(true, false);
    assert_eq!(*log.lock().unwrap(), ["detach"]);
}

#[test]
fn the_state_moves_only_to_an_absolute_path_and_the_lock_lives_in_it() {
    assert_eq!(
        root_of(Some(Path::new("/tmp/vz4"))).unwrap(),
        PathBuf::from("/tmp/vz4")
    );
    let fail = root_of(Some(Path::new("vz4"))).unwrap_err();
    assert!(
        fail.msg
            .as_deref()
            .is_some_and(|m| m.contains("LAB_VZ_STATE=vz4 must be an absolute path")),
        "{:?}",
        fail.msg
    );
    assert_eq!(
        lane_lock_path(Path::new("/c/vz.noindex")),
        PathBuf::from("/c/vz.noindex/aterm-linux.lane.lock")
    );
    let shares = shares(Path::new("/src/aterm"), Path::new("/c/vz.noindex/in"));
    assert_eq!(
        shares
            .iter()
            .map(|s| (s.name.clone(), s.path.display().to_string()))
            .collect::<Vec<_>>(),
        [
            (
                entry_name("repo", Path::new("/src/aterm")),
                "/src/aterm".to_string()
            ),
            ("in".to_string(), "/c/vz.noindex/in".to_string())
        ]
    );
}

#[test]
fn each_checkouts_entries_have_names_of_their_own() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let name = entry_name("repo", &a);
    assert!(
        name.starts_with("repo-") && name.len() == "repo-".len() + 16,
        "{name}"
    );
    assert_eq!(
        entry_name("repo", &a),
        name,
        "the same checkout, the same name"
    );
    assert_ne!(
        entry_name("repo", &b),
        name,
        "another checkout, another name"
    );
    assert_ne!(entry_name("git", &a), name, "another kind, another name");
    // A directory made again at the same path is another identity.
    let inode = |p: &Path| {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(p).unwrap().ino()
    };
    let before = inode(&a);
    std::fs::remove_dir(&a).unwrap();
    std::fs::create_dir(&a).unwrap();
    if inode(&a) != before {
        assert_ne!(entry_name("repo", &a), name);
    }
    // The share's names are one directory each, as the agent's plan takes them.
    let mounted = aterm_vz_agent::mounts::parse("");
    let plan = aterm_vz_agent::mounts::plan(
        &mounted,
        &[aterm_vz_agent::wire::Share {
            name,
            path: b"/src/aterm".to_vec(),
        }],
    );
    assert!(plan.is_ok(), "{plan:?}");
}

#[test]
fn the_lanes_variables_join_the_execs_environment_and_its_program_is_the_argv() {
    let base = vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("HOME".to_string(), "/home/lane.guest".to_string()),
    ];
    let (env, argv) = exec_parts(
        &base,
        &crate::linux_guest::with_env(
            &[
                ("CARGO_TARGET_DIR".into(), "/t".into()),
                ("PATH".into(), "/opt/bin:/usr/bin".into()),
            ],
            &args(&["cargo", "test", "A=B"]),
        ),
    );
    assert_eq!(argv, args(&["cargo", "test", "A=B"]));
    assert_eq!(
        env,
        [
            ("HOME".to_string(), "/home/lane.guest".to_string()),
            ("CARGO_TARGET_DIR".to_string(), "/t".to_string()),
            ("PATH".to_string(), "/opt/bin:/usr/bin".to_string()),
        ],
        "a variable of the lane's is over the base's"
    );
    // The negative controls: an argv that is not `env`, an `env` with an option, and one
    // with no program, each run as given.
    for given in [
        args(&["sh", "-c", "A=1 true"]),
        args(&["env", "-i", "A=1", "true"]),
        args(&["env", "A=1"]),
        args(&["env", "A=1", "-u", "B", "true"]),
    ] {
        assert_eq!(exec_parts(&base, &given), (base.clone(), given.clone()));
    }
}

#[test]
fn a_missing_program_under_the_lanes_variables_is_not_started_never_envs_127() {
    let rig = rig();
    let lane = [("LANE".to_string(), "1".to_string())];
    let env = rig.vz.shell(
        "/",
        &crate::linux_guest::with_env(&lane, &args(&["printenv"])),
        None,
    );
    assert!(env.ok(), "{}", env.status());
    assert!(env.text.lines().any(|l| l == "LANE=1"), "{}", env.text);
    assert!(env.text.lines().any(|l| l == "USER=lane"), "{}", env.text);
    let missing = rig.vz.shell(
        "/",
        &crate::linux_guest::with_env(&lane, &args(&["/nonexistent"])),
        None,
    );
    assert_eq!(
        (missing.code, missing.not_started.as_deref()),
        (
            None,
            Some("/nonexistent: No such file or directory (os error 2)")
        )
    );
    let streamed = rig.vz.shell_streamed(
        "/",
        &crate::linux_guest::with_env(&lane, &args(&["/nonexistent"])),
        "",
    );
    assert!(streamed.not_started.is_some(), "{}", streamed.status());
    assert!(
        logged(&rig).contains(&"exec /nonexistent".to_string()),
        "the agent was asked for the program itself: {:?}",
        logged(&rig)
    );
    // The negative control: an `env` the arm runs as given is env's own 127, which the lift
    // keeps the lane's commands from.
    let as_given = rig
        .vz
        .shell("/", &args(&["env", "-i", "/nonexistent"]), None);
    assert_eq!(
        (as_given.code, as_given.not_started.as_deref()),
        (Some(127), None)
    );
}

#[test]
fn a_bake_seals_only_after_its_guest_powered_itself_off() {
    let rig = rig_stopping(Ok(aterm_vz::helper::POWERED_OFF.into()));
    let log = Arc::clone(&rig.log);
    end_bake(rig.vz).unwrap();
    let log = log.lock().unwrap().clone();
    let tail: Vec<&str> = log
        .iter()
        .filter(|l| !l.starts_with("connect") && !l.starts_with("release"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "exec sudo touch /etc/cloud/cloud-init.disabled",
            "exec sudo fstrim /",
            "exec sudo systemctl daemon-reexec",
            "exec sudo sync",
            "poweroff",
            "wait_stopped",
            "detach",
        ],
        "the session is held until the helper says how the VM stopped"
    );
    // The negative controls: a guest a detector stopped hard, one stopped by a `Stop`, and a
    // session that ended with no word: none seals.
    for stopped in [
        Ok("NOT RUN — the guest stopped answering (no heartbeat for 60 s); VM stopped".into()),
        Ok(
            "the last session closed with keep unset; the guest did not power off (no new \
            console line for 60 s), so it was stopped hard"
                .into(),
        ),
        Err("the helper ended the session".into()),
    ] {
        let rig = rig_stopping(stopped.clone());
        let fail = end_bake(rig.vz).unwrap_err();
        assert_eq!(fail.code, crate::linux::NOT_RUN, "{stopped:?}");
        assert!(
            fail.msg
                .as_deref()
                .is_some_and(|m| m.contains("its disk is not sealed")),
            "{:?}",
            fail.msg
        );
    }
}

#[test]
fn down_after_an_escalated_join_is_not_run_and_sends_no_stop() {
    let hung = ClientError::Hung(
        "NOT RUN — the helper did not answer Attach for 60 s: helper 77839 (unique id 38529422, \
         started 1792841683); it was SIGKILLed by its pid, its unique id checked"
            .into(),
    );
    let stops = std::cell::Cell::new(0);
    let fail = down_after(
        Err(hung),
        |_, _| Ok(()),
        77839,
        || {
            stops.set(stops.get() + 1);
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(fail.code, crate::linux::NOT_RUN);
    assert!(
        fail.msg
            .as_deref()
            .is_some_and(|m| m.contains("SIGKILLed by its pid")),
        "{:?}",
        fail.msg
    );
    assert_eq!(stops.get(), 0, "no Stop can be answered");
    // The negative controls: a join refused for another reason still stops the VM, and a
    // joined one is cleaned first.
    let code = down_after(
        Err(ClientError::Error(
            "helper 1's guest has not said hello yet".into(),
        )),
        |_, _| Ok(()),
        1,
        || {
            stops.set(stops.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!((code, stops.get()), (0, 1));
    let rig = rig();
    let log = Arc::clone(&rig.log);
    let cleaned = std::cell::Cell::new(false);
    let code = down_after(
        Ok(rig.vz),
        |_, home| {
            cleaned.set(home == "/home/lane.guest");
            Ok(())
        },
        1,
        || {
            stops.set(stops.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!((code, cleaned.get(), stops.get()), (0, true, 2));
    assert_eq!(
        log.lock().unwrap().last().map(String::as_str),
        Some("detach")
    );
}

#[test]
fn destroy_deletes_once_no_process_holds_a_disk_and_never_under_a_running_helper() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let files: Vec<PathBuf> = ["live.img", "live.json", "baked.img"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    let make = || {
        for f in &files {
            std::fs::write(f, b"disk").unwrap();
        }
    };
    let hung = || {
        Err(ClientError::Hung(
            "NOT RUN — the helper did not answer Stop for 60 s: helper 35012; it was SIGKILLed \
             by its pid, its unique id checked"
                .into(),
        ))
    };
    // A SIGKILLed helper's VM process holds the live disk for three reads, then exits.
    make();
    let mut reads = 0;
    let gone = destroy_files(
        hung(),
        false,
        &files,
        &mut |p| {
            reads += 1;
            Ok(if reads <= 3 && p.ends_with("live.img") {
                vec![35013]
            } else {
                Vec::new()
            })
        },
        aterm_vz::detect::SPAN,
    )
    .unwrap();
    assert_eq!(gone, files, "one destroy deletes them all");
    assert!(reads > 3, "it waited for the holder: {reads} reads");
    // The negative controls: a helper that still runs keeps every file, its holders never
    // read; a holder that never lets go keeps them all.
    make();
    let fail = destroy_files(
        hung(),
        true,
        &files,
        &mut |_| panic!("no holder is read under a running helper"),
        aterm_vz::detect::SPAN,
    )
    .unwrap_err();
    assert_eq!(fail.code, crate::linux::NOT_RUN);
    assert!(files.iter().all(|f| f.exists()));
    let fail = destroy_files(
        Ok(()),
        false,
        &files,
        &mut |_| Ok(vec![35013]),
        Duration::ZERO,
    )
    .unwrap_err();
    assert!(
        fail.msg
            .as_deref()
            .is_some_and(|m| m.contains("pid 35013 holds") && m.contains("nothing was deleted")),
        "{:?}",
        fail.msg
    );
    assert!(files.iter().all(|f| f.exists()));
}

#[test]
fn out_of_maps_each_end_once() {
    let ran = |ended| Ran {
        ended,
        out: b"o".to_vec(),
        err: b"e".to_vec(),
        secs: 1.5,
    };
    let out = out_of(&ran(Ended::Exit(Exit::Exited(0))));
    assert!(out.ok());
    assert_eq!((out.text.as_str(), out.secs), ("oe", 1.5));
    let out = out_of(&ran(Ended::Exit(Exit::Signaled(15))));
    assert_eq!((out.code, out.signal), (None, Some(15)));
    assert_eq!(out.status(), "ended on signal 15");
    assert!(
        out.text
            .ends_with("(the guest command ended on signal 15)\n")
    );
    let out = out_of(&ran(Ended::Exit(Exit::NotStarted("no cwd".into()))));
    assert_eq!(
        (out.code, out.not_started.as_deref(), out.lost.as_deref()),
        (None, Some("no cwd"), None)
    );
    let out = out_of(&ran(Ended::Lost("gone".into())));
    assert_eq!(
        (out.code, out.not_started.as_deref(), out.lost.as_deref()),
        (None, None, Some("gone"))
    );
}
