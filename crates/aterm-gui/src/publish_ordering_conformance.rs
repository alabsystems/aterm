// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `PublishOrdering` (`aterm_spec::derive::publish_ordering_model`,
//! published ⟹ bound): a discovery graph entry must never name a socket that is
//! not yet listening, because `proxy::sweep_stale_graph` deletes exactly such
//! entries — the sibling-respawn race.
//!
//! Each entry is one instance of the model, projected from the real filesystem
//! and socket, not from any flag the code keeps:
//!
//! * `bound` — a DIAL of the socket the entry NAMES succeeds (the liveness the
//!   sweep itself reads; before the entry exists, the socket it will name);
//! * `published` — the entry's `graph/<sid>` file exists.
//!
//! The run drives the real bind (`bind_control_listener`) and the real
//! publication (`publish_discovery`, the register seam's `publish_session`, the
//! sweep) for three entries — the root session's, a session registered BEFORE
//! the bind (published from the snapshot), and one registered after (published
//! by itself) — and validates every step against the model.
//!
//! `control::spawn` itself needs the event loop, so it is not driven. Its order
//! is not left to it: `publish_discovery` CONSUMES the `BoundControl` only the
//! bind produces and hands back the listener `spawn` serves on, and reads the
//! path every entry names from that same value. So `spawn` cannot publish before
//! its bind, cannot name a socket other than the one it bound, and cannot reach
//! its accept loop without publishing — each of those is a compile error, not a
//! runtime ordering. What that type cannot say — that publishing writes the
//! entries, and only for a listening socket — is what this bind checks.
//!
//! NEGATIVE CONTROL: the original main-thread write (an entry published before
//! the bind) is rejected by the committed model and admitted by `Buggy = 1` —
//! and the real sweep then deletes that entry, which is the race the model
//! exists to forbid.

use std::collections::BTreeMap;
use std::sync::RwLock;

use aterm_session::LaunchNonce;
use aterm_spec::derive::publish_ordering_model;
use aterm_spec::{interp, verify};

use super::*;
use crate::session_store::{SessionStore, test_handle};

type State = BTreeMap<&'static str, i64>;

fn fixture() -> (aterm_tempfile::TempDir, control_auth::SocketPlan) {
    // Keep sun_path short even when the platform temporary root is long.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let sock = dir.path().join("control.sock");
    let plan = control_auth::SocketPlan {
        sock_path: sock.to_str().unwrap().to_string(),
        token_path: control_auth::token_path_for_socket(sock.to_str().unwrap()),
        latest_link: None,
    };
    (dir, plan)
}

/// Project one entry: is it on disk, and is the socket it names (or, before it
/// exists, the one it will name) listening?
fn project(sock_dir: &std::path::Path, sock_path: &str, sid: &SessionId) -> State {
    let entry = crate::proxy::read_graph_entry(sock_dir, sid);
    let named = entry
        .as_ref()
        .map_or(sock_path, |(named, _)| named.as_str());
    [
        ("bound", CtlStream::connect(named).is_ok()),
        ("published", entry.is_some()),
    ]
    .into_iter()
    .map(|(name, fact)| (name, i64::from(fact)))
    .collect()
}

fn conforms(prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        &publish_ordering_model(),
        &[],
        prev,
        next,
        Some(action),
        "discovery publish-ordering conformance",
    )
}

/// One entry's model instance, advanced by observing the real code.
struct Entry {
    name: &'static str,
    sid: SessionId,
    state: State,
}

impl Entry {
    fn new(name: &'static str, sid: SessionId) -> Self {
        Self {
            name,
            sid,
            state: publish_ordering_model().init_state(),
        }
    }

    /// Observe the entry after a real step: either it did not move, or it moved
    /// by exactly `action`.
    fn observe(&mut self, sock_dir: &std::path::Path, sock_path: &str, action: &str) {
        let next = project(sock_dir, sock_path, &self.sid);
        if next == self.state {
            return;
        }
        let (ok, why) = conforms(&self.state, &next, action);
        assert!(
            ok,
            "{} entry: {:?} -> {next:?} is not {action}\n{why}",
            self.name, self.state
        );
        assert!(
            publish_ordering_model().check_invariant("PublishImpliesBound", &next),
            "{} entry names an unbound socket",
            self.name
        );
        self.state = next;
    }
}

/// Clears the process-global discovery state this test sets, even when an
/// assertion fails, so no other test in the binary inherits it.
struct ProcessDiscoveryReset;

impl Drop for ProcessDiscoveryReset {
    fn drop(&mut self) {
        crate::proxy::clear_self_sock();
        crate::proxy::set_mirror_dir_override(None);
    }
}

#[test]
fn discovery_entries_are_published_only_after_the_bind() {
    let _guard = crate::proxy::self_sock_test_guard();
    let mirror = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    crate::proxy::set_mirror_dir_override(Some(mirror.path().to_path_buf()));
    crate::proxy::clear_self_sock();
    let _reset = ProcessDiscoveryReset;

    let model = publish_ordering_model();
    let (_dir, plan) = fixture();
    let sock_path = plan.sock_path.clone();
    let sock_dir = control_auth::dir_of_socket(&sock_path);

    let root = (SessionId::generate(), LaunchNonce::generate());
    let store: Store = Arc::new(RwLock::new(SessionStore::default()));
    let early = test_handle(1);
    let late = test_handle(2);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(early.clone());
    let mut entries = [
        Entry::new("root", root.0.clone()),
        Entry::new("early", early.sid.clone()),
        Entry::new("late", late.sid.clone()),
    ];

    // Before the bind: the register seam's publish must be refused — `Publish`
    // is disabled at Init, and the real entry must not appear.
    crate::proxy::publish_session(&early.sid, &early.nonce);
    for e in &mut entries {
        assert!(!model.action_enabled("Publish", &e.state));
        e.observe(&sock_dir, &sock_path, "Publish");
        assert_eq!(e.state, model.init_state(), "{} published pre-bind", e.name);
    }

    // The real bind: every entry's socket becomes dialable.
    let bound = bind_control_listener(&plan, None, || false, |_| false, None)
        .expect("bind the control socket");
    for e in &mut entries {
        e.observe(&sock_dir, &sock_path, "Bind");
        assert_eq!(e.state["bound"], 1, "{} sees the bind", e.name);
    }

    // The post-bind publication: the root entry and the pre-bind registration.
    let listener = publish_discovery(bound, Some(&root), &store);
    for e in &mut entries {
        e.observe(&sock_dir, &sock_path, "Publish");
    }
    // A session registered after the bind publishes itself.
    crate::proxy::publish_session(&late.sid, &late.nonce);
    for e in &mut entries {
        e.observe(&sock_dir, &sock_path, "Publish");
    }
    // The sweep the ordering protects against must keep every entry.
    crate::proxy::sweep_stale_graph(&sock_dir);
    for e in &mut entries {
        e.observe(&sock_dir, &sock_path, "Publish");
        assert_eq!(
            e.state,
            [("bound", 1), ("published", 1)]
                .into_iter()
                .collect::<State>(),
            "{} entry ends published against a live socket",
            e.name
        );
    }

    drop(listener);
}

/// The defect from a REAL state: an entry written before the bind.
#[test]
fn an_entry_published_before_the_bind_is_the_buggy_step_and_gets_swept() {
    let model = publish_ordering_model();
    let (_dir, plan) = fixture();
    let sock_path = plan.sock_path.clone();
    let sock_dir = control_auth::dir_of_socket(&sock_path);
    let (sid, nonce) = (SessionId::generate(), LaunchNonce::generate());

    let before = project(&sock_dir, &sock_path, &sid);
    crate::proxy::write_graph_entry(&sock_dir, &sid, &sock_path, &nonce);
    let early = project(&sock_dir, &sock_path, &sid);
    assert_eq!(early["published"], 1, "the pre-bind write landed");

    assert!(
        !conforms(&before, &early, "Publish").0,
        "the committed model must reject a pre-bind publish"
    );
    assert!(
        interp::with_buggy(&model, 1)
            .successors("Publish", &before)
            .contains(&early),
        "Buggy = 1 admits exactly this publish"
    );
    assert!(!model.check_invariant("PublishImpliesBound", &early));

    // …and the race it loses: the real sweep deletes an entry whose socket is
    // not listening.
    crate::proxy::sweep_stale_graph(&sock_dir);
    assert!(
        crate::proxy::read_graph_entry(&sock_dir, &sid).is_none(),
        "the sweep removes the entry that was published before its bind"
    );
}
