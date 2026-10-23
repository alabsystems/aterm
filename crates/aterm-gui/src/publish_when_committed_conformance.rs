// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance test binding `publish_when_committed` to
//! `aterm_models_session::handoff_address_owner_model` (`CandidateNamedOnlyAfterCommit`).

use std::path::Path;
use std::sync::RwLock;

use aterm_models_session::handoff_address_owner_model;
use aterm_session::LaunchNonce;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

use super::*;
use crate::session_store::{SessionStore, test_handle};

/// Projects the model state for `sid` from the on-disk discovery entry and `gate`.
pub(super) fn project(
    sock_dir: &Path,
    sid: &SessionId,
    candidate_sock: &str,
    gate: Option<&crate::spawn::DeferredReaderGate>,
) -> State {
    let named =
        crate::proxy::read_graph_entry(sock_dir, crate::proxy::sid_of(sid)).map(|(sock, _)| sock);
    State::from([
        ("cand", 1),
        ("registered", 1),
        ("entry", i64::from(named.as_deref() == Some(candidate_sock))),
        ("pred", i64::from(!gate.is_some_and(|g| g.is_released()))),
        ("asked", 0),
        ("refused", 0),
        ("steps", 0),
    ])
}

fn launched(model: &Model) -> State {
    let mut s = model.init_state();
    assert!(model.fire("Launch", &mut s), "Launch at {s:?}");
    s
}

fn every_invariant_holds(model: &Model, s: &State) -> bool {
    model
        .invariants
        .iter()
        .all(|inv| model.check_invariant(inv.name, s))
}

struct ProcessDiscoveryReset;

impl Drop for ProcessDiscoveryReset {
    fn drop(&mut self) {
        crate::proxy::clear_self_sock();
        let _ = crate::proxy::flush_publications(aterm_spec::wait::MUST_HAPPEN);
        crate::proxy::set_mirror_dir_override(None);
    }
}

struct Candidate {
    _dir: aterm_tempfile::TempDir,
    _mirror: aterm_tempfile::TempDir,
    sock_dir: std::path::PathBuf,
    sock_path: String,
    link: std::path::PathBuf,
    parent_sock: String,
    _parent: std::os::unix::net::UnixListener,
    root: (SessionId, LaunchNonce),
    carried: SessionId,
    store: Store,
    bound: Option<BoundControl>,
}

fn candidate() -> Candidate {
    let mirror = crate::test_scratch::short_dir("the mirror dir");
    crate::proxy::set_mirror_dir_override(Some(mirror.path().to_path_buf()));
    crate::proxy::clear_self_sock();
    let dir = crate::test_scratch::short_dir("the candidate's socket");
    let sock = dir
        .path()
        .join(format!("aterm-{}.sock", std::process::id()));
    let link = dir.path().join("aterm.sock");
    let plan = control_auth::SocketPlan {
        sock_path: sock.to_str().unwrap().to_string(),
        token_path: control_auth::token_path_for_socket(sock.to_str().unwrap()),
        latest_link: Some(link.clone()),
    };
    let sock_dir = control_auth::dir_of_socket(&plan.sock_path);
    let parent_sock = dir
        .path()
        .join("aterm-1.sock")
        .to_str()
        .unwrap()
        .to_string();

    let root = (SessionId::generate(), LaunchNonce::generate());
    let store: Store = Arc::new(RwLock::new(SessionStore::default()));
    let handle = test_handle(1);
    let carried = handle.sid.clone();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    let parent = std::os::unix::net::UnixListener::bind(&parent_sock).expect("the parent's socket");
    for sid in [&root.0, &carried] {
        crate::proxy::write_graph_entry(&sock_dir, sid, &parent_sock, &LaunchNonce::generate());
    }
    let bound = bind_control_listener(&plan, None, |_| true, |_| false, None)
        .expect("bind the candidate's socket");
    Candidate {
        _dir: dir,
        _mirror: mirror,
        sock_dir,
        sock_path: plan.sock_path,
        link,
        parent_sock,
        _parent: parent,
        root,
        carried,
        store,
        bound: Some(bound),
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        drop(self.bound.take());
        drop(ProcessDiscoveryReset);
    }
}

impl Candidate {
    fn ids(&self) -> [SessionId; 2] {
        [self.root.0.clone(), self.carried.clone()]
    }
}

/// Verifies that carried session entries stay on the parent and the `latest` symlink stays
/// unwritten until the Commit gate releases, then transition via `Commit` and `SuccessorPublishes`.
#[test]
fn a_candidates_entries_name_it_only_after_the_commit() {
    let guard = crate::proxy::self_sock_test_guard();
    let _reset = ProcessDiscoveryReset;
    let model = handoff_address_owner_model();
    let mut c = candidate();
    let gate = crate::spawn::DeferredReaderGate::closed();

    let before = launched(&model);
    for sid in c.ids() {
        assert_eq!(
            project(&c.sock_dir, &sid, &c.sock_path, Some(&gate)),
            before,
            "the candidate starts where the model's Launch leaves it"
        );
    }
    assert!(
        !model.action_enabled("SuccessorPublishes", &before),
        "the committed model names no candidate before the Commit"
    );

    let publisher = {
        let (gate, root, store) = (gate.clone(), c.root.clone(), c.store.clone());
        let link = c.link.clone();
        let bound = c.bound.take().expect("bound");
        let lent = guard.lend();
        std::thread::spawn(move || {
            let _state = lent.enter();
            publish_when_committed(bound, Some(&gate), Some(&link), Some(&root), &store)
        })
    };

    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while gate.parked() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the publisher never reached the gate"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    for sid in c.ids() {
        assert_eq!(
            project(&c.sock_dir, &sid, &c.sock_path, Some(&gate)),
            before,
            "a carried id named the candidate while the parent owns it"
        );
        assert_eq!(
            crate::proxy::read_graph_entry(&c.sock_dir, crate::proxy::sid_of(&sid))
                .map(|(sock, _)| sock),
            Some(c.parent_sock.clone()),
            "the parent's entry stands"
        );
    }
    assert!(
        std::fs::symlink_metadata(&c.link).is_err(),
        "the latest alias moved before the Commit"
    );
    assert!(
        !publisher.is_finished(),
        "the publication waits on the Commit"
    );

    gate.release();
    let listener = publisher
        .join()
        .expect("the publisher")
        .expect("a committed candidate publishes");
    let mut committed = before.clone();
    assert!(model.fire("Commit", &mut committed));
    for sid in c.ids() {
        let after = project(&c.sock_dir, &sid, &c.sock_path, Some(&gate));
        for (prev, next, action) in [
            (&before, &committed, "Commit"),
            (&committed, &after, "SuccessorPublishes"),
        ] {
            let (ok, why) = verify::validate_transition_tiered(
                &model,
                &[],
                prev,
                next,
                Some(action),
                "handoff address owner: gated publication",
            );
            assert!(ok, "{prev:?} -> {next:?} is not {action}\n{why}");
        }
        assert_eq!(after["entry"], 1, "the candidate owns the address now");
        assert!(every_invariant_holds(&model, &after), "{after:?}");
    }
    assert_eq!(
        std::fs::read_link(&c.link).expect("the alias, at the Commit"),
        Path::new(&c.sock_path).file_name().unwrap(),
        "the latest alias names the candidate"
    );
    drop(listener);
}

/// Negative control: publishing without a gate before Commit reaches a state rejected by the
/// committed model, admitted by `Early = 1`, and violating `CandidateNamedOnlyAfterCommit`.
#[test]
fn the_ungated_publication_is_the_caught_boot_time_step() {
    let _guard = crate::proxy::self_sock_test_guard();
    let _reset = ProcessDiscoveryReset;
    let model = handoff_address_owner_model();
    let early = interp::with_consts(&model, &[("Early", 1)]);
    let mut c = candidate();
    let gate = crate::spawn::DeferredReaderGate::closed();
    let before = launched(&model);

    let listener = publish_when_committed(
        c.bound.take().expect("bound"),
        None,
        Some(&c.link),
        Some(&c.root),
        &c.store,
    )
    .expect("an ungated launch publishes at once");
    for sid in [c.root.0.clone(), c.carried.clone()] {
        let after = project(&c.sock_dir, &sid, &c.sock_path, Some(&gate));
        assert_eq!(
            (after["entry"], after["pred"]),
            (1, 1),
            "named while the parent owns it"
        );
        let (ok, _) = verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &after,
            Some("SuccessorPublishes"),
            "handoff address owner: ungated publication (committed model)",
        );
        assert!(
            !ok,
            "the committed model must reject a pre-Commit publication"
        );
        assert!(
            early
                .successors("SuccessorPublishes", &before)
                .contains(&after),
            "Early = 1 admits exactly this publication"
        );
        assert!(
            !model.check_invariant("CandidateNamedOnlyAfterCommit", &after),
            "{after:?}"
        );
    }
    drop(listener);
}
