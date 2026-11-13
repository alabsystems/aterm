// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_session::{LaunchNonce, SessionId};

fn dir() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("scratch control dir")
}

const LIVE_FOREIGN_PID: u32 = if cfg!(windows) { 4 } else { 1 };

fn live_foreign_entry() -> String {
    format!("sock /nonexistent/aterm.sock\nnonce ab\npid {LIVE_FOREIGN_PID}\n")
}

#[test]
fn a_second_claim_on_one_id_is_refused_while_the_first_lives() {
    let d = dir();
    let sid = SessionId::generate();
    let first = match claim_in(d.path(), &sid) {
        ClaimOutcome::Held(c) => c,
        _ => panic!("the first claimant must win"),
    };
    assert!(
        matches!(claim_in(d.path(), &sid), ClaimOutcome::Taken),
        "a second live claimant on one id must be refused, not served"
    );
    drop(first);
    assert!(
        matches!(claim_in(d.path(), &sid), ClaimOutcome::Held(_)),
        "a relaunch after the holder exits must adopt its identity"
    );
}

/// A dropped claim is free at once even while a duplicate descriptor on the
/// same open file description (`try_clone`, as a spawning child holds until
/// it execs) is still open.
#[cfg(unix)]
#[test]
fn a_dropped_claim_is_free_while_a_copy_of_its_descriptor_lives() {
    let d = dir();
    let sid = SessionId::generate();
    let ClaimOutcome::Held(first) = claim_in(d.path(), &sid) else {
        panic!("the first claimant must win");
    };
    let childs_copy = first.file.try_clone().expect("dup the claim's descriptor");
    drop(first);
    assert!(
        matches!(claim_in(d.path(), &sid), ClaimOutcome::Held(_)),
        "a dropped claim is free while a copy of its descriptor lives"
    );
    drop(childs_copy);

    let linked_sid = SessionId::generate();
    let outside = d.path().join("outside.txt");
    std::fs::write(&outside, b"keep\n").expect("seed outside");
    let linked_claim = claim_path(d.path(), &linked_sid).expect("claim path");
    std::os::unix::fs::symlink(&outside, &linked_claim).expect("symlink claim");
    assert!(
        matches!(claim_in(d.path(), &linked_sid), ClaimOutcome::Unprovable(_)),
        "a symlinked claim path must be refused as unprovable"
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep\n");
}

#[test]
fn session_id_claim_conformance_real_adoption_projects_onto_model() {
    let _guard = rendezvous_test_guard();
    let d = dir();

    let m = aterm_models_session::session_id_claim_model();
    let mut st = m.init_state();
    let premint = SessionId::generate();

    assert!(
        m.fire("Launch", &mut st),
        "the model admits the first launch"
    );
    let first = claim_for_adoption(d.path(), &premint);
    assert!(first, "and the real first launch adopts the premint");
    assert_eq!(st.get("holders"), Some(&1));

    assert!(m.fire("Launch", &mut st), "a second launch happens");
    let second = claim_for_adoption(d.path(), &premint);
    assert!(
        !second,
        "the real second launch is refused, and mints its own"
    );
    assert_eq!(
        st.get("holders"),
        Some(&1),
        "the model and the code agree: still exactly one holder"
    );
    assert!(m.check_invariant("AtMostOneHolder", &st));

    let mut mh = aterm_models_session::session_id_claim_model();
    for c in &mut mh.consts {
        if c.0 == "Handoff" {
            c.1 = 1;
        }
    }
    let mut st = mh.init_state();
    let handed = SessionId::generate();
    assert!(mh.fire("Launch", &mut st));
    assert!(
        claim_for_adoption(d.path(), &handed),
        "the predecessor adopts"
    );

    release_claims_for_test();
    std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
    std::fs::write(
        d.path().join("graph").join(handed.as_str()),
        live_foreign_entry(),
    )
    .expect("plant the successor's entry");
    assert!(
        mh.fire("PredecessorExits", &mut st),
        "the model admits the predecessor's exit"
    );
    assert!(
        mh.fire("SuccessorPublishes", &mut st),
        "…and the successor's publish"
    );
    assert_eq!(
        st.get("lock"),
        Some(&0),
        "the successor cannot inherit the flock"
    );
    assert_eq!(
        st.get("entry"),
        Some(&1),
        "its own entry is what holds the id"
    );
    assert_eq!(
        st.get("holders"),
        Some(&1),
        "a transfer is not a second holder"
    );

    assert!(mh.fire("Launch", &mut st));
    let after_handoff = claim_for_adoption(d.path(), &handed);
    assert!(
        !after_handoff,
        "the live entry must refuse this launch even though the lock is free — \
         this is the arm a lock-only gate gets wrong"
    );
    assert_eq!(st.get("holders"), Some(&1));
    assert!(mh.check_invariant("AtMostOneHolder", &st));
}

/// Tier-1 binding for the handoff window (`SessionIdClaim`'s `PredecessorExits` /
/// `SuccessorPublishes`) through the real `claim_for_adoption`, `mark_successor_in` and
/// `settle_transferred_in`: with the predecessor gone (no flock, its entry naming a dead pid)
/// and the successor unpublished, only the Commit marker refuses a launch.
#[cfg(unix)]
#[test]
fn the_handoff_window_is_held_by_the_successor_marker() {
    let _guard = rendezvous_test_guard();
    let d = dir();
    let base = aterm_models_session::session_id_claim_model();
    let marked = aterm_spec::interp::with_consts(&base, &[("Handoff", 1)]);
    let unmarked = aterm_spec::interp::with_consts(&base, &[("Handoff", 1), ("Unmarked", 1)]);
    let mut successor = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the successor stand-in");
    let succ = successor.id();
    let dead_predecessor_entry = |sid: &SessionId| {
        std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
        std::fs::write(
            d.path().join("graph").join(sid.as_str()),
            "sock /nonexistent/aterm.sock\nnonce ab\npid 2147483646\n",
        )
        .expect("the dead predecessor's entry");
    };

    for (model, write_marker) in [(&marked, true), (&unmarked, false)] {
        let sid = SessionId::generate();
        let mut st = model.init_state();
        assert!(model.fire("Launch", &mut st));
        assert!(claim_for_adoption(d.path(), &sid), "the predecessor adopts");
        if write_marker {
            mark_successor_in(d.path(), std::slice::from_ref(&sid), succ);
        }
        release_claims_for_test();
        dead_predecessor_entry(&sid);
        assert!(model.fire("PredecessorExits", &mut st));
        assert_eq!(
            (st.get("lock"), st.get("entry")),
            (Some(&0), Some(&0)),
            "the window: no lock, no live entry"
        );
        assert!(model.fire("Launch", &mut st));
        let adopted = claim_for_adoption(d.path(), &sid);
        assert_eq!(
            i64::from(adopted) + 1,
            st["holders"],
            "the real launch and the model agree on the holder count \
             (marker written: {write_marker})"
        );
        if write_marker {
            assert!(!adopted, "the marker refuses the window launch");
            assert!(model.check_invariant("AtMostOneHolder", &st));
        } else {
            assert!(
                adopted,
                "NEGATIVE CONTROL: with no marker the real code adopts a second time"
            );
            assert!(!model.check_invariant("AtMostOneHolder", &st));
            release_claims_for_test();
            continue;
        }
        std::fs::write(
            d.path().join("graph").join(sid.as_str()),
            format!("sock /nonexistent/aterm.sock\nnonce ab\npid {succ}\n"),
        )
        .expect("the successor's entry");
        settle_transferred_in(d.path(), std::slice::from_ref(&sid), succ);
        assert_eq!(
            marked_successor_in(d.path(), &sid),
            None,
            "the successor retires its marker"
        );
        assert!(model.fire("SuccessorPublishes", &mut st));
        assert!(model.fire("Launch", &mut st));
        assert!(
            !claim_for_adoption(d.path(), &sid),
            "after the publish, the live entry (and the taken claim) refuse"
        );
        assert_eq!(st["holders"], 1);
        release_claims_for_test();
    }

    let sid = SessionId::generate();
    mark_successor_in(d.path(), std::slice::from_ref(&sid), 2_147_483_646);
    assert_eq!(live_successor_in(d.path(), &sid), None);
    mark_successor_in(d.path(), std::slice::from_ref(&sid), std::process::id());
    assert_eq!(live_successor_in(d.path(), &sid), None);
    assert!(claim_for_adoption(d.path(), &sid));
    release_claims_for_test();

    let _ = successor.kill();
    let _ = successor.wait();
}

#[test]
fn a_malformed_injected_id_never_becomes_a_path() {
    let d = dir();
    for hostile in [
        "../../escape",
        "s-../../escape",
        "/etc/passwd",
        "s-not-hex-at-all-nope",
        "",
    ] {
        let sid = SessionId::new(hostile);
        assert!(
            claim_path(d.path(), &sid).is_none(),
            "{hostile:?} shaped a path"
        );
        assert!(
            matches!(claim_in(d.path(), &sid), ClaimOutcome::Unprovable(_)),
            "{hostile:?} must be unprovable, never claimable"
        );
        assert!(
            !claim_for_adoption(d.path(), &sid),
            "{hostile:?} must not be adopted"
        );
    }
    assert!(
        !d.path().join(CLAIMS_DIR).join("escape").exists(),
        "no claim file may be created outside the claims directory"
    );
}

#[test]
fn a_live_discovery_entry_holds_an_id_a_dead_one_does_not() {
    let d = dir();
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    assert_eq!(live_holder_in(d.path(), &sid), None, "no entry, no holder");

    crate::proxy::write_graph_entry(d.path(), &sid, "/nonexistent/aterm.sock", &nonce);
    assert_eq!(
        live_holder_in(d.path(), &sid),
        None,
        "an instance must not read its own discovery entry as a rival"
    );

    let foreign = d.path().join("graph").join(sid.as_str());
    std::fs::write(&foreign, live_foreign_entry()).expect("plant a foreign entry");
    assert_eq!(
        live_holder_in(d.path(), &sid),
        Some(LIVE_FOREIGN_PID),
        "a live foreign holder must be reported"
    );
    assert!(
        !claim_for_adoption(d.path(), &sid),
        "an id a live instance publishes must not be adopted, claim file free or not"
    );

    std::fs::write(
        &foreign,
        "sock /nonexistent/aterm.sock\nnonce ab\npid 2147483646\n",
    )
    .expect("plant a dead entry");
    assert_eq!(live_holder_in(d.path(), &sid), None);
    assert!(
        claim_for_adoption(d.path(), &sid),
        "a dead instance's leftover entry must not block a relaunch"
    );
}

/// The candidate registration (bound to `HandoffAddressOwner`'s `Launch` and `Reject`):
/// while it lives, an adopted id whose entry names that pid is served here, and its drop
/// clears only its own pid.
#[cfg(unix)]
#[test]
fn a_handoff_candidate_registration_is_exactly_its_own() {
    let d = dir();
    let _override = rendezvous_override(d.path());
    let mut successor = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the successor stand-in");
    let succ = successor.id();
    let sid = SessionId::generate();
    note_adopted(&sid);
    std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
    let entry = |pid: u32| {
        std::fs::write(
            d.path().join("graph").join(sid.as_str()),
            format!("sock /nonexistent/aterm-{pid}.sock\nnonce ab\npid {pid}\n"),
        )
        .expect("plant the entry");
    };
    entry(succ);
    assert_eq!(registered_handoff_candidate(), 0, "nothing registered yet");
    assert_eq!(
        live_holder(&sid),
        Some(succ),
        "NEGATIVE CONTROL: unregistered, the early entry is refused (the live line)"
    );

    let reg = register_handoff_candidate(succ);
    assert_eq!(registered_handoff_candidate(), succ);
    assert_eq!(live_holder(&sid), None, "the transfer this process decides");
    assert_eq!(
        live_holder_in(d.path(), &sid),
        Some(succ),
        "adoption still sees the candidate as a holder"
    );
    assert!(
        !claim_for_adoption(d.path(), &sid),
        "a launch may not adopt an id the candidate publishes"
    );
    entry(LIVE_FOREIGN_PID);
    assert_eq!(
        live_holder(&sid),
        Some(LIVE_FOREIGN_PID),
        "NEGATIVE CONTROL: a stranger is refused while the registration lives"
    );

    let later = register_handoff_candidate(LIVE_FOREIGN_PID);
    drop(reg);
    assert_eq!(registered_handoff_candidate(), LIVE_FOREIGN_PID);
    drop(later);
    assert_eq!(
        registered_handoff_candidate(),
        0,
        "every registration dropped"
    );
    entry(succ);
    assert_eq!(
        live_holder(&sid),
        Some(succ),
        "NEGATIVE CONTROL: after the drop the live candidate's entry is refused again"
    );

    let _ = successor.kill();
    let _ = successor.wait();
    assert_eq!(
        live_holder(&sid),
        None,
        "a dead candidate's entry holds nothing"
    );
}
