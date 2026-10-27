// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_account_restart_cannot_switch_the_replacement_process() {
    let a = Accts::new();
    let sid = "s-restart-source-replaced";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let world = Arc::clone(&a);
    let kept = Arc::clone(&idle.kept);
    let original = Arc::clone(&idle.hooks.acts.restart);
    let replacement = Arc::new(Mutex::new(None));
    let replaced = Arc::clone(&replacement);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        let new = world.spawn(sid, ACCT_TALK_B, "pool-a");
        kept.lock().unwrap().snapshot = Some(new.clone());
        *replaced.lock().unwrap() = Some(new);
        original(sid, grace, why, selected)
    });
    let _ = idle.restart(&move_to("work-2", AccountAt::Wall));
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(
        a.tabs.lock().unwrap().get(sid),
        replacement.lock().unwrap().as_ref()
    );
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
}

#[test]
fn an_account_restart_cannot_use_a_rebound_target() {
    let a = Accts::new();
    let sid = "s-restart-source-rebound";
    let before = a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let world = Arc::clone(&a);
    let original = Arc::clone(&idle.hooks.acts.restart);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        let text = std::fs::read_to_string(world.roster_path()).unwrap();
        let text = text.replace(
            "label = \"work-2\"",
            "label = \"work-2\"\nemail = \"replacement@example.test\"",
        );
        std::fs::write(world.roster_path(), text).unwrap();
        original(sid, grace, why, selected)
    });
    let _ = idle.restart(&move_to("work-2", AccountAt::Wall));
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.tabs.lock().unwrap().get(sid), Some(&before));
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
}

#[test]
fn a_retired_worker_cannot_apply_its_account_restart() {
    let a = Accts::new();
    let sid = "s-restart-source-stopped";
    let before = a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let stop = Arc::clone(&idle.stop);
    let original = Arc::clone(&idle.hooks.acts.restart);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        stop.store(true, Ordering::SeqCst);
        original(sid, grace, why, selected)
    });
    let _ = idle.restart(&move_to("work-2", AccountAt::Wall));
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.tabs.lock().unwrap().get(sid), Some(&before));
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
}

#[test]
fn an_absent_source_cannot_start_a_new_account_restart() {
    let a = Accts::new();
    let sid = "s-restart-source-absent";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    a.tabs.lock().unwrap().remove(sid);
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(a.asked("restart:"), 0);
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
}
