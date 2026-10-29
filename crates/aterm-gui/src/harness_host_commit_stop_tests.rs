// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The final Commit marks worker stops without re-entering guarded session metadata.

use super::*;

#[test]
fn reserved_commit_stop_waits_outside_claim_guards_and_leaves_unwritten_misses_clean() {
    let app = crate::App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let world = Arc::new(World::default());
    let host = HostHandle::start(off(), false, false, hooks(&world, parking_body(&world)));
    let cuts = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for word in [Word::Watching, Word::Left, Word::Stopped] {
        let left = Arc::new(ExitWord::default());
        if word == Word::Left {
            assert!(left.hand());
        } else if word == Word::Stopped {
            left.stop();
        }
        let (ctx, cuts) = (ctx.clone(), cuts.clone());
        let interrupt: Interrupter = Box::new(move || {
            assert!(
                ctx.meta.try_lock().is_ok(),
                "cleanup runs after the meta guard"
            );
            assert!(
                ctx.timeline.try_lock().is_ok(),
                "cleanup runs after the timeline guard"
            );
            cuts.fetch_add(1, Ordering::SeqCst);
            crate::session_timeline::write_attention_owned(
                &ctx,
                "commit-stop-test",
                crate::session_timeline::MetaEdit::Clear,
                aterm_slog::Actor::Aterm,
            )
            .expect("cleanup may enter the ordinary metadata path now");
        });
        let worker = StopHandle {
            agent: Program::Claude,
            revoked: Arc::default(),
            handover: Arc::default(),
            stop: Arc::new(AtomicBool::new(word == Word::Stopped)),
            left,
            interrupt: Arc::new(Mutex::new(Some(interrupt))),
            kept: Arc::default(),
            left_at_commit: Arc::default(),
        };
        handles.push((
            word,
            worker.stop.clone(),
            worker.left.clone(),
            worker.kept.clone(),
            worker.left_at_commit.clone(),
        ));
        host.shared.lock().stops.push(worker);
    }

    // An aborted inner claim check drops the reservation without mutating host or worker state.
    let missed: Option<()> = host.with_commit_stop_reserved(|_stop| None);
    assert_eq!(missed, None);
    assert!(
        !host.suspended(),
        "a missed reservation changed no host state"
    );
    for (word, stop, left, kept, left_at_commit) in &handles {
        assert_eq!(stop.load(Ordering::SeqCst), *word == Word::Stopped);
        assert_eq!(left.read(), *word);
        assert!(matches!(kept.lock().unwrap().commit, Committed::No));
        assert!(!left_at_commit.load(Ordering::SeqCst));
    }
    assert_eq!(cuts.load(Ordering::SeqCst), 0);

    // Stops are marked inside the claim guards with no callback run.
    let written = host.with_commit_stop_reserved(|stop| {
        let _meta = ctx.meta.try_lock().expect("claim meta lock");
        let _timeline = ctx.timeline.try_lock().expect("claim timeline lock");
        Some(stop.write(|| {
            assert!(matches!(
                host.shared.state.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            for (_, stop, left, kept, _) in &handles {
                assert!(stop.load(Ordering::SeqCst), "the write sees a stop flag");
                assert_eq!(left.read(), Word::Stopped);
                assert!(matches!(
                    kept.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
            }
            assert_eq!(
                cuts.load(Ordering::SeqCst),
                0,
                "no cleanup ran before the write"
            );
            Err::<(), _>("fixture Commit failure")
        }))
    });
    assert_eq!(written, Some(Err("fixture Commit failure")));
    assert!(host.suspended());
    for (word, stop, left, kept, left_at_commit) in &handles {
        assert!(stop.load(Ordering::SeqCst), "the write sees a stop flag");
        assert_eq!(
            left.read(),
            Word::Stopped,
            "the write sees the stopped exit word"
        );
        assert_eq!(left_at_commit.load(Ordering::SeqCst), *word == Word::Left);
        assert_eq!(
            matches!(kept.lock().unwrap().commit, Committed::Stopped),
            *word != Word::Stopped,
            "an already stopped worker keeps its existing custody"
        );
    }
    assert_eq!(
        cuts.load(Ordering::SeqCst),
        0,
        "the stop invoked no cleanup callback"
    );
    host.suspend();
    assert_eq!(cuts.load(Ordering::SeqCst), handles.len());
    host.shutdown_and_join();
}
