// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{metrics, report_gpu_surface_present};
use aterm_gpu::SurfacePresentFailure;

#[test]
fn surface_results_route_and_publish_only_completed_waits() {
    use metrics::PresentDropReason as Reason;
    for (result, wait, want) in [
        (Err(SurfacePresentFailure::AcquirePending), None, Ok(None)),
        (Ok(()), Some(80_000), Ok(Some(()))),
        (
            Err(SurfacePresentFailure::Timeout),
            Some(542_000_000),
            Err(Reason::GpuTimeout),
        ),
        (
            Err(SurfacePresentFailure::Occluded),
            None,
            Err(Reason::GpuOccluded),
        ),
        (
            Err(SurfacePresentFailure::Unavailable),
            None,
            Err(Reason::GpuUnavailable),
        ),
        (
            Err(SurfacePresentFailure::Validation),
            None,
            Err(Reason::GpuValidation),
        ),
    ] {
        let mut samples = Vec::new();
        assert_eq!(
            report_gpu_surface_present(result, wait, |ns| samples.push(ns)),
            want,
            "{result:?}"
        );
        assert_eq!(samples, wait.into_iter().collect::<Vec<_>>(), "{result:?}");
    }

    let mut app = crate::App::headless_for_test();
    let id = crate::WindowId(0);
    for _ in 0..crate::PRESENT_RETRY_CAP {
        let before = crate::Instant::now();
        let reason =
            report_gpu_surface_present(Err(SurfacePresentFailure::Unavailable), None, |_| {
                panic!("no completed acquire")
            })
            .expect_err("a temporarily unavailable surface");
        app.handle_failed_present(id, reason, before);
        let retry = &mut app.windows.get_mut(&id).expect("the window").present_retry;
        let due = retry.deadline.expect("another attempt without input");
        assert!(due > before, "the retry is strictly future");
        assert!(!retry.present_attempt_allowed());
        assert!(retry.take_due(due));
        assert!(retry.present_attempt_allowed());
    }
    app.handle_failed_present(id, Reason::GpuUnavailable, crate::Instant::now());
    let retry = &mut app.windows.get_mut(&id).expect("the window").present_retry;
    assert!(retry.parked, "exhaustion cannot spin");
    assert!(retry.deadline.is_none());
    assert!(retry.on_external_stimulus());
    app.handle_failed_present(id, Reason::GpuValidation, crate::Instant::now());
    let retry = &app.windows[&id].present_retry;
    assert!(retry.parked, "validation still waits for external input");
    assert!(retry.deadline.is_none());
}
