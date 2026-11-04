// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted writers and encrypted transfers for wide-frontier application tests.

use super::Lab;
use crate::table::{Scope, Session, store::Store};
use astream_wire::sync::MAX_PARENTS;
use aterm_secrets::{
    client,
    commons::{
        binding::Pending,
        hearing::{LIFTED, Ledger, Said},
        journal::Journal,
        roster::{Grant, LEGACY_MAX_MEMBERS, Role},
    },
};
use std::{
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub(crate) fn wires(lab: &Lab, writer: usize, scope: Scope) -> Vec<Vec<u8>> {
    let bootstrap = lab.bootstrap(writer);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[writer], bootstrap.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    store
        .inventory(&session)
        .unwrap()
        .map(|id| store.wire(&session, id).unwrap().unwrap().to_vec())
        .collect()
}

pub(crate) fn receive(lab: &Lab, writer: usize, scope: Scope, wires: &[Vec<u8>]) {
    let bootstrap = lab.bootstrap(writer);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[writer], bootstrap.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    for wire in wires {
        store.ingest(&session, wire).unwrap();
    }
}

/// Each writer's first record in `scope`, sealed by that writer's own session: what its first
/// product write appends (no predecessor, no parents), with none of that write's table upkeep. A
/// receiver verifies and interprets it like any other. A test that seals its far writers keeps the
/// last one, past `LEGACY_MAX_MEMBERS`, on the product path, and its control is that test's own.
pub(crate) fn first_records(
    lab: &Lab,
    writers: std::ops::Range<usize>,
    scope: Scope,
    payload: impl Fn(usize, &Session<'_>) -> Vec<u8>,
) -> Vec<Vec<u8>> {
    writers
        .map(|writer| {
            let bootstrap = lab.bootstrap(writer);
            let session = Session::open(&bootstrap).unwrap();
            super::seal(
                &session,
                scope,
                crate::crypto::Causal {
                    sequence: 1,
                    previous: None,
                    parents: &[],
                },
                &payload(writer, &session),
            )
            .unwrap()
        })
        .collect()
}

/// One path under a built lab's home, with its mode and, for a file, its bytes.
enum Entry {
    Dir(u32),
    File(Vec<u8>, u32),
}

/// One built lab, read into memory for [`wide_lab`] to lay down again.
struct Template {
    entries: Vec<(PathBuf, Entry)>,
    signer: String,
    source: PathBuf,
    // Every copy's `source.toml` names this home's vault file by its absolute path, so the home
    // stays until the process exits (`fs_lane::remove_at_exit`).
    _home: aterm_tempfile::TempDir,
}

fn read_tree(root: &Path, rel: &Path, into: &mut Vec<(PathBuf, Entry)>) {
    for entry in std::fs::read_dir(root.join(rel)).unwrap() {
        let entry = entry.unwrap();
        let rel = rel.join(entry.file_name());
        let meta = std::fs::symlink_metadata(entry.path()).unwrap();
        let mode = meta.permissions().mode() & 0o7777;
        if meta.is_dir() {
            into.push((rel.clone(), Entry::Dir(mode)));
            read_tree(root, &rel, into);
        } else {
            assert!(meta.is_file(), "a lab makes no link: {}", rel.display());
            into.push((rel, Entry::File(std::fs::read(entry.path()).unwrap(), mode)));
        }
    }
}

/// `MAX_PARENTS + 1` admitted writers, for a test that needs the finished lab and not a fresh
/// enrollment: its own copy of the Macs of one [`built_wide_lab`] per process, so production
/// enrollment still runs once and no test sees another's writes. The copies are not independent in
/// two places: they name one vault file (`source.toml`), and equal Macs share the content-keyed
/// process caches of `aterm-secrets` (`OBSERVE_COMMONS_CACHE`, `BINDINGS` and others), which hold
/// no Mac's path and answer equal bytes alike. One test reaches that vault today, the activation
/// test, whose `admin_open` only reads it, as another test's would: it would take the `PublishLock`
/// only to put Mac 0's cached copy back over a file it judged stale (unverifiable or older), which
/// this one is not. A test that publishes to the vault, or changes a Mac's vault-derived state so
/// that `admin_open` would put a copy back, writes it for every copy, and calls [`built_wide_lab`]
/// instead, as any other width must.
pub(crate) fn wide_lab(writers: usize) -> Lab {
    static TEMPLATE: OnceLock<Template> = OnceLock::new();
    assert_eq!(writers, MAX_PARENTS + 1, "the template holds this width");
    let built = TEMPLATE.get_or_init(|| {
        let lab = built_wide_lab(writers);
        let mut entries = Vec::new();
        read_tree(lab._home.path(), Path::new(""), &mut entries);
        aterm_spec::fs_lane::remove_at_exit(lab._home.path());
        Template {
            entries,
            signer: lab.signer,
            source: lab.source,
            _home: lab._home,
        }
    });
    aterm_spec::fs_lane::join();
    let home = aterm_tempfile::tempdir().unwrap();
    for (rel, entry) in &built.entries {
        let at = home.path().join(rel);
        match entry {
            Entry::Dir(mode) => std::fs::DirBuilder::new().mode(*mode).create(&at).unwrap(),
            Entry::File(bytes, mode) => {
                std::io::Write::write_all(
                    &mut std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(*mode)
                        .open(&at)
                        .unwrap(),
                    bytes,
                )
                .unwrap();
            }
        }
    }
    let paths = (0..writers)
        .map(|n| client::Paths::for_home(&home.path().join(format!("m{}", n + 1))))
        .collect();
    Lab {
        _home: home,
        paths,
        signer: built.signer.clone(),
        source: built.source.clone(),
    }
}

/// `writers` admitted writers on a lab of their own, enrolled through production: 63 at genesis,
/// the next two by `enroll_commons_member`, the rest in one rotation.
pub(crate) fn built_wide_lab(writers: usize) -> Lab {
    let lab = Lab::with_enrolled(&vec![Role::Writer; writers], LEGACY_MAX_MEMBERS);
    // Each writer's cached bootstrap, read once: its device, request, journal folder and trust
    // are what every step below needs of it, and none of them changes as the roster grows.
    let peers = (0..writers).map(|n| lab.bootstrap(n)).collect::<Vec<_>>();
    let requests = peers
        .iter()
        .map(|peer| peer.request().unwrap())
        .collect::<Vec<_>>();
    let owner = &peers[0];
    let mut ledger = Ledger::new(owner.trust());
    let now = aterm_time::unix_millis().unwrap();
    for request in &requests {
        ledger.record(request.binding().actor(), now, Said::Version(LIFTED));
    }
    ledger.write(owner).unwrap();
    {
        let admin = client::admin_open(&lab.paths[0]).unwrap();
        let recipients = lab.paths[LEGACY_MAX_MEMBERS..]
            .iter()
            .map(|paths| {
                client::machine_identity(paths)
                    .unwrap()
                    .recipient()
                    .to_bech32()
            })
            .collect::<Vec<_>>();
        client::admin_publish_approved(&lab.paths[0], &admin, &admin.doc, &recipients).unwrap();
    }
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let possession = |writer: usize| {
        let request = &requests[writer];
        let (mut pending, challenge) =
            Pending::issue(request, owner.trust().network, owner.trust().authority).unwrap();
        let answer = peers[writer].device().answer(request, &challenge).unwrap();
        pending.verify(&answer).unwrap()
    };
    let mut nodes = Vec::new();
    // The first two rosters past 63 members take production enrollment (enroll_commons_member):
    // the crossing, under its version gate, and the first growth of a roster already past 63,
    // the path by which every later device joins a network that crossed (batching it too would
    // leave a refusal there untested). A history that holds such a roster needs no gate, so the
    // other writers join in one rotation, each by its own proven and approved possession: at 129
    // writers every later open of every writer replays four rosters, not one more per writer
    // past 63 (67).
    let enrolled = writers.min(LEGACY_MAX_MEMBERS + 2);
    for writer in LEGACY_MAX_MEMBERS..enrolled {
        let node = admin
            .enroll_commons_member(owner, &mut journal, possession(writer))
            .unwrap();
        nodes.push(node.bytes().to_vec());
    }
    if writers > enrolled {
        let mut grants = requests[..enrolled]
            .iter()
            .enumerate()
            .map(|(n, request)| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    request.binding().clone(),
                    Role::Writer,
                )
            })
            .collect::<Vec<_>>();
        for writer in enrolled..writers {
            let host = format!("m{}", writer + 1);
            let certificate = admin
                .auto_approve_commons_member(&journal, &host, possession(writer))
                .unwrap();
            grants.push(Grant::certified(host, certificate, Role::Writer));
        }
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        let access = owner.access_in(&journal).unwrap().unwrap();
        assert_eq!(access.members().len(), writers, "every writer is enrolled");
        nodes.push(node.bytes().to_vec());
    }
    drop(journal);
    // Every peer receives the same admitted sequence through one open of its journal.
    for peer in &peers[1..] {
        let mut journal = Journal::open_existing(peer.journal_dir(), peer.trust()).unwrap();
        for node in &nodes {
            journal.ingest(node).unwrap();
        }
    }
    lab
}
