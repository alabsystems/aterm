// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `HandoffAddressOwner`'s order half
//! (`aterm_models_session::handoff_address_owner_model`,
//! `CandidateNamedOnlyAfterCommit`): a per-process update candidate names itself in
//! no `graph/<sid>` entry until the Commit, so one `@<sid>` is never answered by two
//! live instances while the parent still owns the session (a supervisor reads that
//! as `ERR ambiguous session id <sid> also served by pid <new>`).
//!
//! The run drives the real gated publication (`publish_when_committed`, the one
//! function `control::spawn` publishes through) against a real bound socket and
//! discovery directory, projecting the model's state from disk and the gate:
//!
//! * `entry`: 1 when the carried id's `graph/<sid>` names the candidate's socket,
//!   0 while it still names the parent's;
//! * `pred`: 1 until the Commit releases the gate, 0 after;
//! * `cand = registered = 1`: the parent's lane made and registered this candidate
//!   (the model's `Launch`).
//!
//! Negative control: the same publication with no gate writes the step the model
//! rejects and `Early = 1` admits, into a state `CandidateNamedOnlyAfterCommit`
//! refuses.

use std::path::Path;
use std::sync::RwLock;

use aterm_models_session::handoff_address_owner_model;
use aterm_session::LaunchNonce;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

use super::*;
use crate::session_store::{SessionStore, test_handle};

/// The model's state for one carried id, read off the real discovery entry and
/// the real Commit gate. See the module header.
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

/// The model's state right after `Launch` — the candidate exists, the parent
/// owns everything.
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

/// Clears the process-global discovery state this test sets, even when an
/// assertion fails, so no other test in the binary inherits it.
struct ProcessDiscoveryReset;

impl Drop for ProcessDiscoveryReset {
    fn drop(&mut self) {
        crate::proxy::clear_self_sock();
        // A write queued against the test's socket lands while its mirror
        // override still stands, never in the real control dir.
        let _ = crate::proxy::flush_publications(aterm_spec::wait::MUST_HAPPEN);
        crate::proxy::set_mirror_dir_override(None);
    }
}

/// A candidate booted with two carried ids, its root session's and one more in the
/// store, each with the parent's entry already on disk, and its own socket bound.
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
    let mirror = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    crate::proxy::set_mirror_dir_override(Some(mirror.path().to_path_buf()));
    crate::proxy::clear_self_sock();
    // Keep sun_path short even when the platform temporary root is long.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
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
    // The parent, listening: it still serves both ids (and the candidate's bind
    // sweeps any entry whose socket does not answer, so a dead parent socket
    // would leave nothing to observe).
    let parent = std::os::unix::net::UnixListener::bind(&parent_sock).expect("the parent's socket");
    // The parent's entries: it published them, and it still owns both ids.
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

impl Candidate {
    fn ids(&self) -> [SessionId; 2] {
        [self.root.0.clone(), self.carried.clone()]
    }
}

/// Through the whole pre-Commit window the carried ids' entries still name the
/// parent and the `latest` alias does not move (`SuccessorPublishes` is disabled
/// there). The Commit releases the gate, and the real publication is the model's
/// `Commit` then `SuccessorPublishes`, into a state every invariant holds.
#[test]
fn a_candidates_entries_name_it_only_after_the_commit() {
    let _guard = crate::proxy::self_sock_test_guard();
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
        std::thread::spawn(move || {
            publish_when_committed(bound, Some(&gate), Some(&link), Some(&root), &store)
        })
    };

    // THE PRE-COMMIT WINDOW: once the publisher is parked on the closed gate
    // (a fact, awaited by a hang detector — never a guessed window), every
    // carried entry still names the parent, and nothing moved.
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

    // The Commit: the gate opens, and the candidate publishes.
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

/// Negative control, from a real state: with no gate the publication names the
/// candidate while the parent still owns the carried ids. The committed model rejects
/// that step, `Early = 1` admits exactly it, and `CandidateNamedOnlyAfterCommit`
/// refuses the state it reaches.
#[test]
fn the_ungated_publication_is_the_caught_boot_time_step() {
    let _guard = crate::proxy::self_sock_test_guard();
    let _reset = ProcessDiscoveryReset;
    let model = handoff_address_owner_model();
    let early = interp::with_consts(&model, &[("Early", 1)]);
    let mut c = candidate();
    // The parent has not committed: its gate stays shut throughout.
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
