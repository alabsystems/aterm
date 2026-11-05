// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn retained_case(cold: bool) {
    use aterm_agent::harness::relaunch::{AgentRun, CodexRun};
    use aterm_agent::harness::self_claimed::Resume;
    let sid = if cold {
        "s-cold-admission"
    } else {
        "s-retained-admission"
    };
    let base = snap(sid);
    let session = base.session.clone().unwrap();
    let original = Snapshot {
        program: "/opt/bin/codex".into(),
        argv: vec!["/opt/bin/codex".into()],
        run: AgentRun::Codex(CodexRun {
            home: "/tmp/codex-home".into(),
            home_env: None,
            self_claimed: Some(Resume {
                argv: vec!["/opt/bin/codex".into(), "resume".into(), session.clone()],
                session,
                codex_home: None,
                codex_auth_path: Some("/tmp/cold-account/auth.json".into()),
                cwd: None,
            }),
        }),
        ..base
    };
    let world = Arc::new(World::default());
    let allowed = Arc::new(AtomicBool::new(false));
    let observations = Arc::new(AtomicUsize::new(0));
    let launched = Arc::new(Mutex::new(Vec::new()));
    let mut h = hooks(&world, parking_body(&world));
    h.pause = Arc::new(|_| Duration::from_millis(1));
    let admitted = Arc::clone(&allowed);
    let seen = Arc::clone(&observations);
    h.acts.codex_cold = Arc::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        if admitted.load(Ordering::SeqCst) {
            Ok(None)
        } else {
            Err("prepared account awaits execution authority".into())
        }
    });
    let recorded = Arc::clone(&launched);
    h.acts.relaunch_restored = Arc::new(move |_, snapshot, _, _, _| {
        recorded.lock().unwrap().push(snapshot.clone());
        "adopted".into()
    });
    let host = HostHandle::start(on(), false, false, h);
    host.relaunch_restored(vec![RestoredAgent {
        sid: sid.into(),
        snap: original.clone(),
        place: "in test tab".into(),
        cold,
        asked: None,
        was: None,
    }]);
    until("account hold outlives ordinary retries", || {
        observations.load(Ordering::SeqCst) > RESTORED_TRIES as usize + 1
    });
    assert!(launched.lock().unwrap().is_empty());
    allowed.store(true, Ordering::SeqCst);
    until("pending conversation resumes after admission", || {
        !launched.lock().unwrap().is_empty()
    });
    host.shutdown_and_join();
    let launched = launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    let resume = |snapshot: &Snapshot| snapshot.codex().unwrap().self_claimed.clone().unwrap();
    assert_eq!(resume(&launched[0]), resume(&original));
}

#[test]
fn a_cold_restore_keeps_its_conversation_until_account_admission_recovers() {
    retained_case(true);
    retained_case(false);
}
