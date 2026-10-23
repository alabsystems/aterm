// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless integration tests for fleet accounts upkeep under a Seatbelt network witness.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;

use crate::headless_boot::{self, Instance};

/// The network witness's jail (SBPL, the last rule that matches wins): a name
/// lookup (the resolver's socket), or an IP connection to anything but IPv4
/// loopback, kills the process that asked, at once. The resolver's rule comes
/// first: after the loopback allowance it also kills an established loopback
/// connection (macOS 27).
const NETWORK_JAIL: &str = concat!(
    "(version 1)(allow default)",
    "(deny network-outbound (literal \"/private/var/run/mDNSResponder\") ",
    "(with send-signal SIGKILL))",
    "(deny network-outbound (remote ip) (with send-signal SIGKILL))",
    "(allow network-outbound (remote ip \"localhost:*\"))",
);

/// The programs the upkeep fetches a source with.
const FETCHERS: [&str; 2] = ["curl", "rclone"];

/// Each pid seen under this test process: its parent, and every program path
/// it was seen running.
type Seen = BTreeMap<i32, (i32, BTreeSet<PathBuf>)>;

/// What every process under this test process ran, read every millisecond
/// from [`Execs::watch`] until dropped ([`Seen`]).
struct Execs {
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Execs {
    /// Start reading this test process's tree.
    fn watch() -> Self {
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (into, until) = (Arc::clone(&seen), Arc::clone(&stop));
        let root = i32::try_from(std::process::id()).expect("this test's pid");
        let thread = std::thread::spawn(move || {
            while !until.load(Ordering::Relaxed) {
                let mut frontier = vec![root];
                while let Some(parent) = frontier.pop() {
                    for kid in aterm_uds::process::child_pids(parent).unwrap_or_default() {
                        if let Some(path) = aterm_uds::process::read_process_path(kid) {
                            let mut seen = into.lock().unwrap();
                            let row = seen.entry(kid).or_insert_with(|| (parent, BTreeSet::new()));
                            row.0 = parent;
                            row.1.insert(path);
                        }
                        frontier.push(kid);
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            seen,
            stop,
            thread: Some(thread),
        }
    }

    /// Every program path a process under `root` (the instance) was seen
    /// running whose file name is one of `names`.
    fn under(&self, root: u32, names: &[&str]) -> Vec<PathBuf> {
        let seen = self.seen.lock().unwrap();
        let root = i32::try_from(root).expect("a pid");
        let mut tree = BTreeSet::from([root]);
        loop {
            let before = tree.len();
            for (pid, (parent, _)) in seen.iter() {
                if tree.contains(parent) {
                    tree.insert(*pid);
                }
            }
            if tree.len() == before {
                break;
            }
        }
        seen.iter()
            .filter(|(pid, _)| tree.contains(pid))
            .flat_map(|(_, (_, paths))| paths.iter())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| names.contains(&name))
            })
            .cloned()
            .collect()
    }
}

impl Drop for Execs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The private state root the seam's admission asks for (`ATERM_STATE_HOME`),
/// under an instance's scratch root.
const PRIVATE_STATE: &str = "state-aterm";

/// The instance's own log (`aterm.log` where its scratch home keeps logs, or
/// its private state root where it was given one).
fn instance_log(inst: &Instance) -> String {
    [None, Some(inst.tmp.join(PRIVATE_STATE))]
        .into_iter()
        .filter_map(|root| {
            aterm_types::dirs::resolve_logs_dir(
                root,
                aterm_types::dirs::StatePlatform {
                    home: Some(inst.tmp.join("home")),
                    xdg_state_home: Some(inst.tmp.join("state")),
                    ..Default::default()
                },
            )
        })
        .filter_map(|dir| std::fs::read_to_string(dir.join("aterm.log")).ok())
        .collect()
}

/// The last lines of the instance's own log, for a failure message.
fn log_tail(inst: &Instance) -> String {
    let log = instance_log(inst);
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(25)..].join("\n")
}

/// The upkeep's first word in the instance's log (`accounts-upkeep=…`),
/// waited for under the hang detector.
#[track_caller]
fn upkeep_word(inst: &mut Instance) -> String {
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let log = instance_log(inst);
        if let Some(line) = log.lines().find(|l| l.contains("accounts-upkeep=")) {
            return line.to_owned();
        }
        alive(inst);
        assert!(
            Instant::now() < until,
            "the accounts' upkeep never said what it found; log tail:\n{}",
            log_tail(inst)
        );
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}

/// The instance still runs: the jail killed it for no connection or lookup.
#[track_caller]
fn alive(inst: &mut Instance) {
    let status = inst.child.try_wait().expect("the instance's status");
    assert!(
        status.is_none(),
        "the instance died ({status:?}): the network jail kills a process at its first \
         non-loopback connection or name lookup; gui.log tail:\n{}",
        headless_boot::log_tail(&inst.log)
    );
}

/// Boot a jailed instance whose aterm.toml gets `extra` appended, with the
/// live upkeep let in by its development seam where `live` says so (under the
/// private state root the seam's admission asks for).
#[track_caller]
fn boot(prefix: &str, extra: String, live: bool) -> Instance {
    headless_boot::boot_jailed(
        prefix,
        NETWORK_JAIL,
        move |tmp: &Path, cmd| {
            let toml = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&toml).expect("aterm.toml");
            std::fs::write(&toml, format!("{base}{extra}")).expect("aterm.toml");
            cmd.env("ATERM_LOG", "info");
            if live {
                cmd.env("ATERM_STATE_HOME", tmp.join(PRIVATE_STATE))
                    .env(aterm_agent::harness::accounts_sync::UPKEEP_SEAM, "1");
            }
        },
        |_| true,
    )
}

/// `[fleet] accounts_source` naming the loopback recorder on `port`.
fn loopback_config(port: u16) -> String {
    format!("[fleet]\naccounts_source = \"http://127.0.0.1:{port}/fleet.csv\"\nmachine = \"m1\"\n")
}

/// Wait, under the hang detector, for the instance's log to say its
/// activation wake is armed: the upkeep's first kick is behind it.
#[track_caller]
fn wake_armed(inst: &mut Instance) {
    let until_at = Instant::now() + MUST_HAPPEN;
    while !instance_log(inst).contains("the activation wake is armed") {
        alive(inst);
        assert!(
            Instant::now() < until_at,
            "the activation wake was never armed; log tail:\n{}",
            log_tail(inst)
        );
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}

/// A loopback source that answers one GET: what it was asked, sent on the
/// first channel, and its answer, `body`, written once the second says so.
fn loopback_source(
    body: &'static str,
) -> (
    u16,
    std::sync::mpsc::Receiver<String>,
    std::sync::mpsc::Sender<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its port").port();
    let (asked_tx, asked) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        let line = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let _ = asked_tx.send(line);
        if released.recv_timeout(MUST_HAPPEN).is_ok() {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/csv\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, asked, release)
}

/// Wait, under the hang detector, until `ready` holds.
#[track_caller]
fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + MUST_HAPPEN;
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "{what}: not within the hang detector"
        );
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}

#[test]
fn a_test_instance_and_a_fresh_home_fetch_no_accounts_and_a_configured_one_fetches_its_own() {
    let execs = Execs::watch();
    let no_files = |inst: &Instance| {
        let cfg = inst.tmp.join("cfg/aterm");
        for file in ["accounts.toml", "fleet-accounts.toml"] {
            assert!(!cfg.join(file).exists(), "{file} written");
        }
    };

    let (port, asked, _release) = loopback_source("email,computer,claude\nm1@example.test,m1,1\n");
    let mut inst = boot("acct-upkeep-inert", loopback_config(port), false);
    let word = upkeep_word(&mut inst);
    assert!(word.contains("accounts-upkeep=inert"), "{word}");
    wake_armed(&mut inst);
    alive(&mut inst);
    assert!(
        asked.try_recv().is_err(),
        "the inert instance asked its configured source"
    );
    let ran = execs.under(inst.child.id(), &FETCHERS);
    assert!(ran.is_empty(), "the inert instance ran {ran:?}");
    no_files(&inst);
    drop(inst);

    let mut inst = boot("acct-upkeep-fresh", String::new(), true);
    let word = upkeep_word(&mut inst);
    assert!(word.contains("accounts-upkeep=unconfigured"), "{word}");
    alive(&mut inst);
    let ran = execs.under(inst.child.id(), &FETCHERS);
    assert!(ran.is_empty(), "the unconfigured instance ran {ran:?}");
    no_files(&inst);
    drop(inst);

    let (port, asked, release) = loopback_source("email,computer,claude\nm1@example.test,m1,1\n");
    let mut inst = boot("acct-upkeep-conf", loopback_config(port), true);
    let line = asked.recv_timeout(MUST_HAPPEN).unwrap_or_else(|_| {
        panic!(
            "the configured source was not fetched within the hang detector; log tail:\n{}",
            log_tail(&inst)
        )
    });
    assert!(line.starts_with("GET /fleet.csv "), "{line}");
    let pid = inst.child.id();
    until("the fetch's curl seen running under the instance", || {
        execs
            .under(pid, &["curl"])
            .iter()
            .any(|p| p == Path::new("/usr/bin/curl"))
    });
    release.send(()).expect("the source's answer");
    let roster = inst.tmp.join("cfg/aterm/accounts.toml");
    until("the roster the source answered", || {
        std::fs::read_to_string(&roster).is_ok_and(|r| r.contains("m1@example.test"))
    });
    let word = upkeep_word(&mut inst);
    assert!(
        word.contains("accounts-upkeep=configured source=key"),
        "{word}"
    );
    alive(&mut inst);
}
