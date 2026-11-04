// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS LOG AT REST, on the real broker (`aterm_link::at_rest`): a `rest.key` beside the log
//! seals it, an existing plaintext log is sealed at the start that finds a key, and a sealed log
//! whose key is gone refuses, naming the way out, until a person gives it up.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::harness::{self, FLEET, World};

const SENTINEL: &[u8] = b"QZREST-sentinel-of-the-world";
const KEY_NAME: &str = "rest.key";

fn scratch() -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atr-")
        .tempdir_in("/tmp")
        .expect("scratch dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("0700");
    dir
}

fn log_of(dir: &Path) -> PathBuf {
    dir.join("b.log")
}

fn plant_key(dir: &Path, key: &[u8; 32]) {
    aterm_uds::private_file::write_private(dir, KEY_NAME, key).expect("the key");
}

fn said(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("broker.err")).unwrap_or_default()
}

fn holds_sentinel(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes.windows(SENTINEL.len()).any(|w| w == SENTINEL))
}

fn spawn(dir: &Path) -> harness::BrokerChild {
    let err = std::fs::File::create(dir.join("broker.err")).expect("broker.err");
    let sock = dir.join("b.sock").to_string_lossy().into_owned();
    let log = log_of(dir).to_string_lossy().into_owned();
    harness::spawn_broker(dir, &[&sock, &log, "--durability", "ordered"], |cmd| {
        cmd.stdout(aterm_uds::spawnfd::Stdio::piped()).stderr(err);
    })
}

/// A broker's run: it serves, takes one record per `seq`, and stops by SIGTERM. What it said.
fn serve_and_publish(dir: &Path, seqs: std::ops::RangeInclusive<u64>) -> String {
    let mut broker = spawn(dir);
    let first = harness::first_line_within(&mut broker.child, "its readiness line");
    assert!(first.starts_with("listening "), "{first}: {}", said(dir));
    let mut client = harness::broker_client(dir.join("b.sock"));
    for seq in seqs {
        client
            .publish(5, seq, "/o/a", SENTINEL)
            .expect("the record is acked");
    }
    drop(client);
    harness::kill(i32::try_from(broker.child.id()).expect("pid"), 15);
    harness::until("the broker to stop", || {
        broker.child.try_wait().ok().flatten()
    });
    said(dir)
}

/// A start that refuses: its exit status is failure, and what it said.
fn refused_start(dir: &Path) -> String {
    let mut broker = spawn(dir);
    let status = harness::until("the broker to refuse", || {
        broker.child.try_wait().ok().flatten()
    });
    assert!(!status.success(), "it stopped refusing: {}", said(dir));
    said(dir)
}

fn link(args: &[&str]) -> (bool, String, String) {
    let out = harness::broker_command(harness::aterm_to_launch(), args)
        .output()
        .expect("run the broker's verb");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn records(log: &Path, key: [u8; 32]) -> usize {
    let opened = astream_broker::BrokerLog::open_encrypted_bound(log, key).expect("it opens");
    opened.read_from(opened.base()).len()
}

/// NEGATIVE CONTROL first: the broker with no key leaves the sentinel in its log and says
/// `at-rest=plaintext`. The key beside it: the start seals that log, record for record, keeps
/// its identity, and says so; a later record is sealed too.
#[test]
fn a_broker_seals_a_plaintext_log_at_the_start_that_finds_a_key() {
    let scratch = scratch();
    let dir = scratch.path();
    let log = log_of(dir);
    let plain = serve_and_publish(dir, 1..=2);
    assert!(plain.contains("at-rest=plaintext"), "{plain}");
    assert!(holds_sentinel(&log), "CONTROL: the plaintext log holds it");
    let identity = {
        let opened = astream_broker::BrokerLog::open(&log).expect("plaintext opens");
        opened.identity().to_string()
    };

    plant_key(dir, &harness::REST_KEY);
    let sealing = serve_and_publish(dir, 3..=4);
    assert!(sealing.contains("sealed at rest under"), "{sealing}");
    assert!(sealing.contains("at-rest=sealed"), "{sealing}");
    assert!(
        sealing.contains(&format!("log {identity}")),
        "its identity is kept: {identity}\n{sealing}"
    );
    assert!(!holds_sentinel(&log), "no sentinel in the sealed log");
    assert_eq!(records(&log, harness::REST_KEY), 4, "every record reads");
    // The start line says what of the log it keeps decoded (D10): aterm sets no residency
    // budget, so every record it found.
    let (resident, retained) = resident_of(&sealing).expect("the start line's residency");
    assert!(
        retained >= 2 && resident == retained,
        "{resident} of {retained}: {sealing}"
    );
}

/// The `resident=<records>/<bytes> of <retained>` a broker's start line names: its records and
/// its retained.
fn resident_of(start: &str) -> Option<(u64, u64)> {
    let rest = start.split(" resident=").nth(1)?;
    let (records, rest) = rest.split_once('/')?;
    let retained = rest.split(" of ").nth(1)?.split(';').next()?;
    Some((records.parse().ok()?, retained.parse().ok()?))
}

/// A sealed log with its key gone: the broker refuses, naming the key and `--retire-log`, and the
/// log's bytes stay; `--repair` refuses too. The key back, it serves and repairs. Given up, the
/// log is kept aside and the next start is a new log with a new identity and a new key.
#[test]
fn a_sealed_log_without_its_key_refuses_until_it_is_given_up() {
    let scratch = scratch();
    let dir = scratch.path();
    let log = log_of(dir);
    let log_word = log.to_string_lossy().into_owned();
    plant_key(dir, &harness::REST_KEY);
    let first = serve_and_publish(dir, 1..=3);
    assert!(first.contains("at-rest=sealed"), "{first}");
    assert!(!holds_sentinel(&log));
    let bytes = std::fs::read(&log).expect("the log");

    let key = dir.join(KEY_NAME);
    std::fs::remove_file(&key).expect("lose the key");
    let refused = refused_start(dir);
    assert!(
        refused.contains("sealed at rest")
            && refused.contains(&*key.to_string_lossy())
            && refused.contains("--retire-log"),
        "{refused}"
    );
    assert_eq!(std::fs::read(&log).expect("the log"), bytes, "unchanged");
    let (ok, _, err) = link(&["--repair", &log_word]);
    assert!(!ok, "a keyless repair runs: {err}");
    assert!(err.contains("--retire-log"), "{err}");
    assert_eq!(std::fs::read(&log).expect("the log"), bytes, "unchanged");

    plant_key(dir, b"not-the-key-this-log-was-sealed!");
    let refused = refused_start(dir);
    assert!(refused.contains("--retire-log"), "{refused}");
    assert_eq!(std::fs::read(&log).expect("the log"), bytes, "unchanged");
    let (ok, _, err) = link(&["--repair", &log_word]);
    assert!(!ok, "a repair under another key runs: {err}");
    assert_eq!(std::fs::read(&log).expect("the log"), bytes, "nothing cut");

    std::fs::remove_file(&key).expect("remove the wrong key");
    plant_key(dir, &harness::REST_KEY);
    let (ok, out, err) = link(&["--repair", &log_word]);
    assert!(ok, "the keyed repair: {err}");
    assert!(out.contains("nothing to repair"), "{out}");
    let (ok, _, err) = link(&["--retire-log", &log_word]);
    assert!(!ok, "a log that opens is retired");
    assert!(err.contains("nothing to give up"), "{err}");
    let identity = {
        let opened =
            astream_broker::BrokerLog::open_encrypted_bound(&log, harness::REST_KEY).expect("it");
        opened.identity().to_string()
    };

    std::fs::remove_file(&key).expect("lose the key for good");
    let (ok, out, err) = link(&["--retire-log", &log_word]);
    assert!(ok, "it retires: {err}");
    assert!(out.contains("retired to"), "{out}");
    let aside: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("list")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("retired-"))
        })
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(
        std::fs::read(aside[0].join("b.log")).expect("kept aside"),
        bytes,
        "the sealed records are kept"
    );
    assert!(aside[0].join("b.log.seal").exists());
    let new_key: [u8; 32] = std::fs::read(&key)
        .expect("a new key")
        .try_into()
        .expect("32 bytes");
    assert_ne!(new_key, harness::REST_KEY);

    let next = serve_and_publish(dir, 1..=1);
    assert!(next.contains("at-rest=sealed"), "{next}");
    assert!(!next.contains(&identity), "a new identity: {next}");
    assert_eq!(records(&log, new_key), 1);
}

/// The printed way out works as printed, for a broker started with a bare relative log. A folder,
/// which a person may type for the log, goes nowhere, with the broker's log inside it.
#[test]
fn retire_log_takes_a_bare_name_and_refuses_a_folder() {
    let scratch = scratch();
    let dir = scratch.path();
    plant_key(dir, &harness::REST_KEY);
    serve_and_publish(dir, 1..=1);
    std::fs::remove_file(dir.join(KEY_NAME)).expect("lose the key");

    let folder = dir.to_string_lossy().into_owned();
    let (ok, _, err) = link(&["--retire-log", &folder]);
    assert!(!ok, "a folder retired: {err}");
    assert!(err.contains("is not a file"), "{err}");
    assert!(log_of(dir).exists());

    // A retire a crash stopped: a start and a repair refuse, naming the verb, and mint nothing.
    let marker = dir.join("b.log.retiring");
    std::fs::write(&marker, "retired-1").expect("the marker");
    let refused = refused_start(dir);
    assert!(
        refused.contains("did not finish") && refused.contains("--retire-log"),
        "{refused}"
    );
    let log_word = log_of(dir).to_string_lossy().into_owned();
    let (ok, _, err) = link(&["--repair", &log_word]);
    assert!(!ok && err.contains("--retire-log"), "{err}");
    assert!(!dir.join(KEY_NAME).exists(), "no key minted");
    std::fs::remove_file(&marker).expect("the marker");

    let out = harness::broker_command(harness::aterm_to_launch(), &["--retire-log", "b.log"])
        .current_dir(dir)
        .output()
        .expect("run the broker's verb");
    let (said, err) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{err}");
    assert!(said.contains("retired to ./retired-"), "{said}");
    assert!(!log_of(dir).exists(), "{said}");
    assert!(dir.join(KEY_NAME).exists(), "a new key waits: {said}");
}

/// Mail through a world whose log is sealed: delivered, delivered again across the broker's
/// restart, and in no file the broker keeps. The plaintext arm finds it (the negative control).
#[test]
fn mail_crosses_a_sealed_log_and_stays_out_of_its_bytes() {
    use aterm_link::durability::Durability;
    for sealed in [false, true] {
        let opts = harness::broker_opts(Durability::Ordered);
        let opts = if sealed {
            harness::sealed_opts(opts)
        } else {
            opts
        };
        let mut w = World::boot_arm("at-rest", opts, true, &[], &[]);
        w.wait_ready();
        let (a, _b) = w.two_sessions();
        let subject = format!("/f/{FLEET}/in/{}/{a}/h-rest/note", w.node);
        let body =
            |n: u32| format!("v=1 t=1 text={}-{n}", String::from_utf8_lossy(SENTINEL)).into_bytes();
        let mail = |producer, n| vec![(producer, 1, subject.clone(), body(n))];
        harness::publish_pipelined(&mut w.god(), &mail(7_100, 1));
        harness::until("the first mail", || (!w.inbox(&a).is_empty()).then_some(()));

        w.stop_broker();
        let files: Vec<PathBuf> = std::fs::read_dir(&w.tmp)
            .expect("list")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("b.log"))
            })
            .collect();
        let found: Vec<&PathBuf> = files.iter().filter(|p| holds_sentinel(p)).collect();
        if sealed {
            assert!(
                found.is_empty(),
                "the sealed arm's bytes hold it: {found:?}"
            );
            assert!(w.tmp.join("b.log.seal").exists());
            let keyless = astream_broker::BrokerLog::open(&w.broker_log)
                .err()
                .expect("a keyless open refuses");
            assert_eq!(keyless.kind(), std::io::ErrorKind::InvalidData, "{keyless}");
        } else {
            assert!(!found.is_empty(), "CONTROL: the plaintext arm holds it");
        }
        w.start_broker();
        harness::publish_pipelined(&mut w.god(), &mail(7_101, 2));
        harness::until("the second mail, after the restart", || {
            (w.inbox(&a).len() >= 2).then_some(())
        });
    }
}
