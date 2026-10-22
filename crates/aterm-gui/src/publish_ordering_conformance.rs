// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `PublishOrdering` (`aterm_models_effects::publish_ordering_model`,
//! published ⟹ bound): a discovery graph entry never names a socket that is not yet
//! listening, because `proxy::sweep_stale_graph` deletes exactly such entries.
//!
//! Each entry is one model instance, projected from the real filesystem and socket:
//! `bound` is whether a dial of the socket the entry names (before it exists, the
//! one it will name) succeeds, the liveness the sweep reads; `published` is whether
//! its `graph/<sid>` file exists. The run drives the real bind
//! (`bind_control_listener`), publication (`publish_discovery`, `publish_session`,
//! whose queued write each step waits for) and sweep for three entries: the root
//! session's, one registered before the bind (published from the snapshot) and one
//! registered after.
//!
//! `control::spawn` needs the event loop and is not driven; its order is fixed by
//! types: `publish_discovery` consumes the `BoundControl` only the bind produces and
//! returns the listener `spawn` serves on, so `spawn` cannot publish before its bind,
//! name another socket, or serve without publishing. What the types cannot say, that
//! publishing writes the entries and only for a listening socket, is what this checks.
//!
//! Negative control: an entry published before the bind is rejected by the model,
//! admitted by `Buggy = 1`, and deleted by the real sweep.

use std::collections::BTreeMap;
use std::sync::RwLock;

use aterm_models_effects::publish_ordering_model;
use aterm_session::LaunchNonce;
use aterm_spec::{interp, verify};

use super::*;
use crate::session_store::{SessionStore, test_handle};

type State = BTreeMap<&'static str, i64>;

fn fixture() -> (aterm_tempfile::TempDir, control_auth::SocketPlan) {
    let dir = crate::test_scratch::short_dir("the control socket");
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
    let entry = crate::proxy::read_graph_entry(sock_dir, crate::proxy::sid_of(sid));
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

/// Clears the discovery state this test sets (its own,
/// `proxy::self_sock_test_guard`), even when an assertion fails: its queue
/// drains while its mirror override still stands.
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

/// [`crate::proxy::publish_session`], then its queued write landed: what the
/// next observation reads is the publication's own step.
fn publish_session_landed(sid: &SessionId, nonce: &LaunchNonce) {
    crate::proxy::publish_session(sid, nonce);
    assert!(
        crate::proxy::flush_publications(aterm_spec::wait::MUST_HAPPEN),
        "the publication thread never answered"
    );
}

#[test]
fn discovery_entries_are_published_only_after_the_bind() {
    let _guard = crate::proxy::self_sock_test_guard();
    let mirror = crate::test_scratch::short_dir("the mirror dir");
    crate::proxy::set_mirror_dir_override(Some(mirror.path().to_path_buf()));
    crate::proxy::clear_self_sock();
    // Made before the reset so it drops after it: the reset's flush lands every
    // queued write while the directory stands, not in a re-created `<dir>/graph`.
    let (_dir, plan) = fixture();
    let _reset = ProcessDiscoveryReset;

    let model = publish_ordering_model();
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
    publish_session_landed(&early.sid, &early.nonce);
    for e in &mut entries {
        assert!(!model.action_enabled("Publish", &e.state));
        e.observe(&sock_dir, &sock_path, "Publish");
        assert_eq!(e.state, model.init_state(), "{} published pre-bind", e.name);
    }

    // The real bind: every entry's socket becomes dialable.
    let bound = bind_control_listener(&plan, None, |_| true, |_| false, None)
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
    publish_session_landed(&late.sid, &late.nonce);
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

/// THE STORE IS LET GO BEFORE THE DISK (LAG-5): `publish_discovery` writes the
/// snapshot's entries with the session store released, so a main-thread
/// `store.write()` (a registration, the harness posture) never waits on a graph
/// write a volume-wide flush is holding. A FIFO where the early session's
/// mirror entry goes holds that write open until a reader comes; its own
/// entry, written just before, says the publisher is in the loop. The store's
/// write lock is free meanwhile. NEGATIVE CONTROL: the snapshot taken in the
/// `for` header kept its read guard for the whole loop, and `try_write` failed.
#[cfg(unix)]
#[test]
fn the_snapshot_entries_are_written_with_the_store_let_go() {
    let guard = crate::proxy::self_sock_test_guard();
    let mirror = crate::test_scratch::short_dir("the mirror dir");
    crate::proxy::set_mirror_dir_override(Some(mirror.path().to_path_buf()));
    crate::proxy::clear_self_sock();
    // Before the reset, as in `discovery_entries_are_published_only_after_the_bind`.
    let (_dir, plan) = fixture();
    let _reset = ProcessDiscoveryReset;

    let sock_dir = control_auth::dir_of_socket(&plan.sock_path);
    let store: Store = Arc::new(RwLock::new(SessionStore::default()));
    let early = test_handle(1);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(early.clone());
    let graph = mirror.path().join("graph");
    control_auth::ensure_private_dir(&graph).expect("the mirror's graph dir");
    let fifo = graph.join(early.sid.as_str());
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0, "mkfifo");

    let bound = bind_control_listener(&plan, None, |_| false, |_| false, None)
        .expect("bind the control socket");
    let publishing = Arc::clone(&store);
    let (published, done) = std::sync::mpsc::channel();
    let lent = guard.lend();
    std::thread::spawn(move || {
        let _state = lent.enter();
        let _ = published.send(publish_discovery(bound, None, &publishing));
    });
    let in_loop = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(1),
        || crate::proxy::read_graph_entry(&sock_dir, crate::proxy::sid_of(&early.sid)).map(|_| ()),
    );
    assert!(in_loop.is_some(), "the early entry was written");
    let free = store.try_write().is_ok();
    // A reader lets the held write through either way. Non-blocking, so the
    // open returns whether or not the publisher's write ever opens the FIFO.
    let reader = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("open the FIFO to read")
    };
    let listener = done
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the publisher finished");
    drop((reader, listener));
    assert!(free, "the store was held across the graph write");
}

/// An entry written before the bind, from a real state: the `Buggy = 1` step.
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
        crate::proxy::read_graph_entry(&sock_dir, crate::proxy::sid_of(&sid)).is_none(),
        "the sweep removes the entry that was published before its bind"
    );
}
