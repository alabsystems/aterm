// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An activation received during an owed continuation survives that continuation's point.

use super::*;

#[test]
fn an_activation_consumed_by_a_continued_carry_is_revisited_without_another_idle_event() {
    for due_after in [Due::Yes, Due::No, Due::Unread("fixture-unread")] {
        let world = Arc::new(World::default());
        let owed = Arc::new(AtomicBool::new(true));
        let stepped = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let (o, r, d) = (Arc::clone(&owed), Arc::clone(&owed), Arc::clone(&owed));
        let s = Arc::clone(&stepped);
        let n = Arc::clone(&reads);
        let mut h = hooks(&world, parking_body(&world));
        h.acts = Acts {
            owed: Arc::new(move |_, _| o.load(Ordering::SeqCst)),
            resume: Arc::new(move |_, _, _| {
                r.store(false, Ordering::SeqCst);
                "continued".into()
            }),
            due: Arc::new(move |_| {
                n.fetch_add(1, Ordering::SeqCst);
                assert!(
                    !d.load(Ordering::SeqCst),
                    "eligibility is reread after continuation"
                );
                due_after
            }),
            step: Arc::new(move |_, _, _| {
                s.fetch_add(1, Ordering::SeqCst);
                UpgradeStep::unknown("wait:settling")
            }),
            ..Acts::inert()
        };
        let idle = idle_host("s-carry-activation", Program::Claude, switched(&on()), h);
        idle.activations.note();
        let carried = idle
            .at_idle()
            .expect("the owed continuation takes this point");
        assert_eq!(carried.line, "carry-on step=continued");
        assert_eq!(
            idle.run.lock().unwrap().activations,
            idle.activations.of(Program::Claude)
        );
        assert_eq!(stepped.load(Ordering::SeqCst), 0);
        assert!(
            reads.load(Ordering::SeqCst) > 0,
            "continuation revisits current eligibility"
        );
        let look = *idle.look_at.lock().unwrap();
        if due_after == Due::No {
            assert_eq!(look, None, "no eligible work creates no timer");
            assert!(idle.park.load(Ordering::SeqCst));
            continue;
        }
        assert!(
            look.is_some(),
            "the activation's upgrade gets its own scheduled point"
        );
        // Advance the existing scheduled-look seam, without another activation or turn.
        *idle.look_at.lock().unwrap() = Some(Instant::now() - Duration::from_millis(1));
        assert!(idle.wants());
        let moved = idle
            .at_idle()
            .expect("the scheduled point takes the upgrade");
        if matches!(due_after, Due::Unread(_)) {
            assert_eq!(moved.line, "upgrade step=wait:fixture-unread");
            assert_eq!(stepped.load(Ordering::SeqCst), 0);
            assert!(idle.look_at.lock().unwrap().is_some());
            continue;
        }
        assert_eq!(moved.line, "upgrade step=wait:settling");
        assert_eq!(stepped.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn an_ordinary_upgrade_continued_step_still_waits_for_the_next_idle_event() {
    let world = Arc::new(World::default());
    let reads = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&reads);
    let mut h = hooks(&world, parking_body(&world));
    h.acts = Acts {
        due: Arc::new(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            Due::Yes
        }),
        step: Arc::new(|_, _, _| UpgradeStep::unknown("continued")),
        ..Acts::inert()
    };
    let idle = idle_host("s-ordinary-continued", Program::Claude, switched(&on()), h);
    let step = idle.at_idle().expect("ordinary upgrade step");
    assert_eq!(step.line, "upgrade step=continued");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(*idle.look_at.lock().unwrap(), None);
    assert!(idle.park.load(Ordering::SeqCst));
}
