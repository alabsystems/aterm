// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_walk_holds_its_token_until_the_work_returns() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut ctx = Ctx::new(
        dir.path().into(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_pool(Some(
        aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 1).unwrap(),
    ));
    ctx.child_ceiling = Some(std::time::Duration::from_secs(60));
    let pool = ctx.pool.as_ref().unwrap();
    for stage in [crate::plan::StageId::Census, crate::plan::StageId::Forge] {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (leave_tx, leave_rx) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                crate::compile_width::run_stage(
                    stage,
                    Some(pool.as_ref()),
                    Some(1),
                    None,
                    &|| false,
                    || {
                        bounded_source(&ctx, move || {
                            entered_tx.send(()).unwrap();
                            assert!(
                                aterm_uds::output::current().is_some(),
                                "child commands retain custody"
                            );
                            leave_rx
                                .recv_timeout(std::time::Duration::from_secs(60))
                                .unwrap();
                            42
                        })
                    },
                )
            });
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            assert!(
                pool.try_acquire().unwrap().is_none(),
                "the actual walk owns the token"
            );
            leave_tx.send(()).unwrap();
            assert_eq!(worker.join().unwrap().unwrap().unwrap(), 42);
        });
        assert!(
            pool.try_acquire().unwrap().is_some(),
            "the completed walk returned its token"
        );
    }
}
