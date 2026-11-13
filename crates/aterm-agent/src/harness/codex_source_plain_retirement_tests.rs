// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn dedicated_cleanup_retires_only_exact_empty_or_unrelated_occurrence() {
    // Metadata lifecycle fixture: real observed process, App and connections;
    // no supported image, source admission or credential proof is fabricated.
    let process = Process::observe(std::process::id()).unwrap();
    let caller = slot::Caller {
        sid: "s-plain-cleanup".into(),
        pid: process.pid,
        birth: process.birth,
    };
    let peer = || Peer {
        caller: caller.clone(),
        process: process.clone(),
        launch: None,
    };
    let lifetime = super::super::super::account_work::Lifetime::default();
    let registry = Registry::for_app(&lifetime);
    let work = registry.fixture_connection();
    let cleanup = registry.fixture_connection();
    let open = "a".repeat(32);
    let empty = registry.open_member(&peer(), &work, &open).unwrap().member;
    assert_eq!(
        registry.retire_nonpreloaded_member(&caller, &cleanup, &empty, Some(&"b".repeat(32))),
        Err(Error::Stale)
    );
    assert!(
        registry.state.lock().unwrap().actors[&actor_key(&caller)]
            .issued
            .contains_key(&empty)
    );
    registry
        .retire_nonpreloaded_member(&caller, &cleanup, &empty, None)
        .unwrap();
    // A concurrent replay must not observe Closed during the outside-lock
    // drop interval, even though the issued ordinal has already disappeared.
    let empty_key = member_key(&caller, &empty).unwrap();
    registry
        .state
        .lock()
        .unwrap()
        .plain_retiring
        .insert(empty_key.clone(), ());
    assert_eq!(
        registry.retire_nonpreloaded_member(&caller, &cleanup, &empty, None),
        Err(Error::Busy)
    );
    registry
        .state
        .lock()
        .unwrap()
        .plain_retiring
        .remove(&empty_key);
    // Lost ACK: repeat observes definitive native closure. It cannot allocate
    // a member, return a Source, or change the original work connection.
    registry
        .retire_nonpreloaded_member(&caller, &cleanup, &empty, None)
        .unwrap();
    let api_open = "c".repeat(32);
    let api = registry
        .open_member(&peer(), &work, &api_open)
        .unwrap()
        .member;
    let registration = registry
        .complete_unrelated(peer(), &work, &api, &api_open, NoNativeReaderKind::ApiKey)
        .unwrap();
    // The same no-file-reader completion is used by API managers and direct
    // API readers; neither gets counterfeit ReaderLoaded/manager counters.
    assert_eq!(
        registry.retire_nonpreloaded_member(&caller, &cleanup, &api, None),
        Err(Error::Stale)
    );
    assert_eq!(
        registry.retire_nonpreloaded_member(&caller, &cleanup, &api, Some(&"d".repeat(32))),
        Err(Error::Stale)
    );
    let mut foreign = caller.clone();
    foreign.sid.push_str("-foreign");
    assert!(
        registry
            .retire_nonpreloaded_member(&foreign, &cleanup, &api, Some(&registration))
            .is_err()
    );
    let mut reused = caller.clone();
    reused.birth = reused.birth.saturating_add(1);
    assert!(
        registry
            .retire_nonpreloaded_member(&reused, &cleanup, &api, Some(&registration))
            .is_err()
    );
    registry
        .retire_nonpreloaded_member(&caller, &cleanup, &api, Some(&registration))
        .unwrap();
    registry
        .retire_nonpreloaded_member(&caller, &cleanup, &api, Some(&registration))
        .unwrap();
    let unknown = format!(
        "{}{:016x}",
        registry.state.lock().unwrap().actors[&actor_key(&caller)].epoch,
        u64::MAX
    );
    assert!(
        registry
            .retire_nonpreloaded_member(&caller, &cleanup, &unknown, None)
            .is_err()
    );
    assert!(
        registry.state.lock().unwrap().actors[&actor_key(&caller)]
            .issued
            .is_empty()
    );
    let pause = lifetime.pause().unwrap();
    let _drained = match pause.try_wait() {
        Ok(value) => value,
        Err(_) => panic!("cleanup left native work counted"),
    };
    // After App sealing only the already-Closed observation is legal.
    registry
        .retire_nonpreloaded_member(&caller, &cleanup, &api, Some(&registration))
        .unwrap();
}
