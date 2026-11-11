// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_headless_warm_keeps_its_policy_separate_from_a_windowed_warm() {
    static HEADLESS: WarmLatch = WarmLatch::new(DEADLINE);
    static WINDOWED: WarmLatch = WarmLatch::new(DEADLINE);
    let headless = HEADLESS
        .begin_with_policy(crate::DevicePolicy::Headless)
        .unwrap();
    let windowed = WINDOWED.begin().unwrap();
    assert_eq!(headless.device_policy, crate::DevicePolicy::Headless);
    assert_eq!(windowed.device_policy, crate::DevicePolicy::Windowed);
    assert!(
        HEADLESS
            .begin_with_policy(crate::DevicePolicy::Headless)
            .is_none()
    );
    drop(headless);
    assert_eq!(HEADLESS.current_phase(), Phase::Settled);
    assert_ne!(WINDOWED.current_phase(), Phase::Settled);
    drop(windowed);
    assert_eq!(WINDOWED.current_phase(), Phase::Settled);
}
use crate::metal::test_support::device;
use std::sync::mpsc;

/// The one minute a wait for something that must happen gets.
const HANG: Duration = Duration::from_secs(60);

/// Wait (a hang detector) until `latch` has `n` callers blocked in
/// `await_item`.
fn until_waiting(latch: &WarmLatch, n: usize) {
    let began = Instant::now();
    while latch.waiting.load(std::sync::atomic::Ordering::SeqCst) < n {
        assert!(began.elapsed() < HANG, "no caller ever waited on the warm");
        aterm_uds::deadline::poll_yield();
    }
}

/// A warm whose first compile fails releases every wait on it as it
/// fails and reports the failure; a second warm on the same latch is
/// refused.
#[test]
fn a_failed_warm_releases_the_window_and_is_not_retried() {
    static OK: WarmLatch = WarmLatch::new(DEADLINE);
    let ok_warm = OK.begin().expect("the positive control's first warm");
    OK.start(Lane::Present, WarmItem::Library(ShaderLibrary::Blit));
    assert_eq!(
        OK.run_lane(|| {
            OK.finish(Lane::Cell, WarmItem::Library(ShaderLibrary::Cell));
            Ok(())
        }),
        Ok(())
    );
    assert!(matches!(
        OK.current_phase(),
        Phase::Running {
            in_flight: [None, Some(WarmItem::Library(ShaderLibrary::Blit))],
            ..
        }
    ));
    drop(ok_warm);
    assert_eq!(OK.current_phase(), Phase::Settled);

    static ERROR: WarmLatch = WarmLatch::new(DEADLINE);
    let error_warm = ERROR.begin().expect("the error control's first warm");
    ERROR.start(Lane::Present, WarmItem::Library(ShaderLibrary::Blit));
    let error = ERROR.run_lane(|| {
        ERROR.finish(Lane::Cell, WarmItem::Library(ShaderLibrary::Cell));
        Err("captured cell failure".to_owned())
    });
    assert_eq!(error, Err("captured cell failure".to_owned()));
    let error_phase = ERROR.current_phase();
    drop(error_warm);

    static PANIC: WarmLatch = WarmLatch::new(DEADLINE);
    let panic_warm = PANIC.begin().expect("the unwind control's first warm");
    PANIC.start(Lane::Present, WarmItem::Library(ShaderLibrary::Blit));
    let panicked =
        std::panic::catch_unwind(|| PANIC.run_lane(|| panic!("captured present failure")));
    assert!(panicked.is_err());
    let panic_phase = PANIC.current_phase();
    drop(panic_warm);
    assert!(
        error_phase == Phase::Settled && panic_phase == Phase::Settled,
        "GPU_LANE_FAILURE_MUST_RELEASE_OTHER_LANE_WAIT"
    );

    static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
    let dev = device();
    let mut warm = LATCH.begin().expect("the latch's first warm");
    assert!(matches!(
        LATCH.current_phase(),
        Phase::Running {
            in_flight: [Some(WarmItem::Library(ShaderLibrary::Cell)), _],
            ..
        }
    ));
    let failed = warm.run_on(&dev, BENCH_KNOBS, |_, _| {
        Err("injected compile failure".to_owned())
    });
    assert_eq!(failed, Err("injected compile failure".to_owned()));
    assert_eq!(
        LATCH.current_phase(),
        Phase::Settled,
        "the failure released every wait, the blit it owed too"
    );
    LATCH.await_item(WarmItem::Library(ShaderLibrary::Blit));
    drop(warm);
    assert_eq!(LATCH.current_phase(), Phase::Settled);
    assert!(LATCH.begin().is_none(), "one warm per latch");
    assert!(LATCH.began());
}

/// A warm dropped before it ran (its thread never spawned, or panicked)
/// releases the first window.
#[test]
fn a_warm_dropped_unrun_releases_the_window() {
    static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
    assert!(!LATCH.began(), "no warm, nothing to wait for");
    LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
    drop(LATCH.begin().expect("the latch's first warm"));
    assert_eq!(LATCH.current_phase(), Phase::Settled);
}

/// The window waits for the libraries the warm owes (`cell.metal` and `blit.metal`, from
/// the claim on) and for anything else in flight, and nothing more: held inside the cell
/// compile, a cell wait ends when that compile returns and a blit wait only when the blit
/// compile does; held inside the blit compile, a wait on a finished library, or on one
/// the warm has not reached and does not owe, returns at once.
#[test]
fn the_window_waits_for_what_the_warm_owes_or_has_in_flight() {
    static LATCH: WarmLatch = WarmLatch::new(HANG);
    device();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (blit_tx, blit_rx) = mpsc::channel::<()>();
    let (finish_tx, finish_rx) = mpsc::channel::<()>();
    let (go_rx, finish_rx) = (Mutex::new(go_rx), Mutex::new(finish_rx));
    let mut warm = LATCH.begin().expect("the latch's first warm");
    let worker = std::thread::spawn(move || {
        let _pool = AutoreleasePool::new();
        let dev = Device::preferred_listed().expect("the listed device the test found");
        warm.run_on(&dev, FirstFrameKnobs::default(), |dev, lib| {
            let held = |rx: &Mutex<mpsc::Receiver<()>>, what: &str| {
                rx.lock()
                    .expect("the test's channel")
                    .recv_timeout(HANG)
                    .unwrap_or_else(|_| panic!("the test's {what}"));
            };
            match lib {
                ShaderLibrary::Cell => held(&go_rx, "go"),
                _ => {
                    blit_tx.send(()).expect("the test is listening");
                    held(&finish_rx, "finish");
                }
            }
            pipelines::compile_library(dev, lib)
        })
    });
    let cell_window = std::thread::spawn(|| {
        LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
        LATCH.current_phase()
    });
    let blit_window = std::thread::spawn(|| {
        LATCH.await_item(WarmItem::Library(ShaderLibrary::Blit));
        LATCH.current_phase()
    });
    until_waiting(&LATCH, 2);
    go_tx.send(()).expect("the warm is listening");
    assert!(
        !matches!(
            cell_window.join().expect("the cell window"),
            Phase::Running {
                in_flight: [Some(WarmItem::Library(ShaderLibrary::Cell)), _],
                ..
            }
        ),
        "released once the cell compile returned"
    );
    blit_rx
        .recv_timeout(HANG)
        .expect("the warm went on to the present library");
    assert!(
        !blit_window.is_finished(),
        "the blit library is owed while the warm compiles it"
    );
    LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
    LATCH.await_item(WarmItem::Library(ShaderLibrary::Bloom));
    finish_tx.send(()).expect("the warm is listening");
    assert!(
        !matches!(
            blit_window.join().expect("the blit window"),
            Phase::Running {
                in_flight: [_, Some(WarmItem::Library(ShaderLibrary::Blit))],
                ..
            }
        ),
        "released once the blit compile returned"
    );
    worker
        .join()
        .expect("the warm thread")
        .expect("the shipped shaders compile");
}

/// The warm compiles and builds every library and row it names on the
/// shipped sources: a row or format it cannot build would fail every launch.
#[test]
fn the_warm_builds_the_shipped_first_frame_shaders() {
    static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
    let dev = device();
    let mut warm = LATCH.begin().expect("the latch's first warm");
    warm.run_on(&dev, BENCH_KNOBS, pipelines::compile_library)
        .expect("the first-frame warm on the shipped shaders");
    assert!(!LATCH.probe_pending(), "the warm ran the probe it owed");
    let cell = pipelines::compile_library(&dev, ShaderLibrary::Cell).expect("cell.metal");
    assert!(
        crate::metal::fetch::known(&dev, &cell).is_some(),
        "the warm left the device's verdict for the window to read"
    );
}

/// The warm owes the framebuffer-fetch verdict from the claim until its probe ran: pending
/// while owed or in flight, not once the probe finished, before a warm began, or after it
/// ended.
#[test]
fn the_probe_is_pending_from_the_claim_until_the_warm_ran_it() {
    static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
    assert!(!LATCH.probe_pending(), "no warm began");
    let warm = LATCH.begin().expect("the latch's first warm");
    assert!(LATCH.probe_pending(), "owed from the claim");
    LATCH.finish(Lane::Cell, WarmItem::Library(ShaderLibrary::Cell));
    LATCH.start(Lane::Cell, WarmItem::FetchProbe);
    assert!(LATCH.probe_pending(), "under way");
    LATCH.finish(Lane::Cell, WarmItem::FetchProbe);
    assert!(!LATCH.probe_pending(), "the probe ran");
    drop(warm);
    assert!(!LATCH.probe_pending());

    static ENDED: WarmLatch = WarmLatch::new(DEADLINE);
    drop(ENDED.begin().expect("the latch's first warm"));
    assert!(!ENDED.probe_pending(), "a warm that ended owes nothing");
}

/// With every present switch on, a pipeline warm names every row of the table at every
/// destination its present meets and every framebuffer-fetch twin, and names what a
/// first-frame warm builds last. A switch turned off drops its pass's row alone (the EDR
/// crown's takes the f16 destinations with it), and no twins drops the twins alone.
#[test]
fn the_pipeline_warm_names_every_row_and_twin_a_frame_can_bind() {
    let on = PresentKnobs {
        bloom: true,
        shimmer: true,
        hdr_glow: true,
        sdr_glow: true,
    };
    let all = bindable(on, true);
    for row in ALL_PIPELINES {
        assert!(
            all.iter()
                .any(|i| matches!(i, WarmItem::Pipeline(r, _) if *r == row)),
            "`{}` is not warmed",
            row.name()
        );
    }
    for twin in FetchTwin::ALL {
        assert!(
            all.contains(&WarmItem::Twin(twin)),
            "{twin:?}'s twin is not warmed"
        );
    }
    let first: Vec<WarmItem> = first_frame_rows(EVERY_FIRST_FRAME, true)
        .into_iter()
        .map(|(row, present)| WarmItem::pipeline(row, present))
        .chain(
            first_frame_twins(EVERY_FIRST_FRAME, true)
                .into_iter()
                .map(WarmItem::Twin),
        )
        .collect();
    let at = all
        .iter()
        .position(|i| first.contains(i))
        .expect("the first frames' rows are bindable");
    assert!(
        all[at..].iter().all(|i| first.contains(i)) && at > 0,
        "the first frames' rows come last: {all:?}"
    );
    let f16 = |i: &WarmItem| matches!(i, WarmItem::Pipeline(_, PixelFormat::Rgba16Float));
    for (off, rows) in [
        (
            PresentKnobs { bloom: false, ..on },
            &[Pipeline::Bloom, Pipeline::GlowItems][..],
        ),
        (
            PresentKnobs {
                shimmer: false,
                ..on
            },
            &[Pipeline::Shimmer],
        ),
        (
            PresentKnobs {
                hdr_glow: false,
                ..on
            },
            &[Pipeline::HdrGlow, Pipeline::HdrGlowItems],
        ),
        (
            PresentKnobs {
                sdr_glow: false,
                ..on
            },
            &[Pipeline::SdrGlow, Pipeline::SdrGlowItems],
        ),
    ] {
        let left = bindable(off, true);
        let dropped: Vec<&WarmItem> = all.iter().filter(|i| !left.contains(i)).collect();
        assert!(
            rows.iter().all(|row| {
                dropped
                    .iter()
                    .any(|i| matches!(i, WarmItem::Pipeline(r, _) if r == row))
            }) && dropped.iter().all(|i| {
                matches!(i, WarmItem::Pipeline(r, _) if rows.contains(r))
                    || (!off.hdr_glow && f16(i))
            }),
            "{off:?} dropped {dropped:?}"
        );
        assert!(
            off.hdr_glow || !left.iter().any(f16),
            "no f16 swapchain, no f16 destination"
        );
    }
    let untwinned = bindable(on, false);
    assert!(
        all.iter()
            .all(|i| untwinned.contains(i) == !matches!(i, WarmItem::Twin(_))),
        "no twins drops the twins alone"
    );
}

/// A pipeline warm hands over each item as it lands, built on the shipped shaders (the
/// framebuffer-fetch probe settled for the process), and its worker ends once it built
/// them; a worker whose arm is gone builds the one item in hand and stops.
#[test]
fn the_pipeline_warm_hands_over_each_build_and_stops_when_its_arm_is_gone() {
    static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
    let dev = device();
    let on = PresentKnobs {
        bloom: true,
        shimmer: true,
        hdr_glow: true,
        sdr_glow: true,
    };
    let mut items = vec![WarmItem::FetchProbe];
    items.extend(bindable(on, super::super::fetch::capable(&dev)));
    let (tx, rx) = mpsc::channel();
    let worker = spawn(&dev, &LATCH, items.clone(), tx).expect("the worker starts");
    let mut handed = Vec::new();
    while handed.len() < items.len() {
        let Warmed { item, pso } = rx.recv_timeout(HANG).expect("the warm hands over");
        assert!(
            pso.is_some() || item == WarmItem::FetchProbe,
            "{item:?} did not build on the shipped shaders"
        );
        handed.push(item);
    }
    assert_eq!(handed, items, "handed over in order, each once");
    assert!(
        matches!(
            rx.recv_timeout(HANG),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "the worker ended once it built its items"
    );
    assert_eq!(worker.join().expect("the worker"), items.len());
    let cell = pipelines::compile_library(&dev, ShaderLibrary::Cell).expect("cell.metal");
    assert!(
        crate::metal::fetch::known(&dev, &cell).is_some(),
        "the warm left the device's verdict for an arm to read"
    );

    let (tx, rx) = mpsc::channel();
    drop(rx);
    let gone = spawn(&dev, &LATCH, items, tx).expect("the worker starts");
    let began = Instant::now();
    while !gone.is_finished() {
        assert!(began.elapsed() < HANG, "the armless worker never stopped");
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(
        gone.join().expect("the worker"),
        1,
        "a worker whose arm is gone stops at its first hand-over"
    );
}
