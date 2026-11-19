// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::present::CpuPresenter as _;
use crate::scripted_redraw_fixture::{feed, glass, scripted_app, surface};
use crate::{App, PresentTarget, WindowId};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const WID: WindowId = WindowId(0);

fn app() -> App {
    let mut app = scripted_app();
    app.perf_reduced = true;
    app.perf_flip_at = Some(Instant::now());
    app.perf_shed_dwell = Duration::MAX;
    app.redraw_window(WID);
    let state = &app.windows[&WID];
    let size = app.window_frame_px_for(WID, state.rows, state.cols);
    app.windows.get_mut(&WID).unwrap().set_window_pixels(size);
    feed(&app, b"captured A\r\n\x1b[?25l");
    // Direct fixture input has no PTY Output wake; carry its ordinary content
    // admission into the real present, including the frame-cap clock it books.
    app.windows.get_mut(&WID).unwrap().content_pending = true;
    app.redraw_window(WID);
    app
}

fn docked_app() -> App {
    let mut app = app();
    let now = Instant::now();
    app.set_dock_at(WID, 30, now)
        .expect("room for the real Navigator");
    app.publish_navigators(now);
    app.redraw_window(WID);
    app
}

fn image(app: &mut App, lease: &Lease) -> Result<(usize, usize, Vec<u8>), String> {
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_image(crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::in_memory(),
        clean: false,
        session: None,
        want_bytes: true,
        want_metadata: false,
        whole_client: true,
        presentation: Some(lease.clone()),
        frame_metadata: Arc::new(std::sync::OnceLock::new()),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    });
    let (_, _, png) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the capture worker answers")?
        .value;
    let (rgba, width, height) =
        aterm_render::decode_png_rgba8(&png.expect("PNG bytes")).expect("the capture is a PNG");
    Ok((width, height, rgba))
}

fn rgba(pixels: &[u32]) -> Vec<u8> {
    pixels
        .iter()
        .flat_map(|&pixel| {
            [
                ((pixel >> 16) & 255) as u8,
                ((pixel >> 8) & 255) as u8,
                (pixel & 255) as u8,
                255,
            ]
        })
        .collect()
}

#[test]
fn a_presentation_lease_holds_the_real_dock_and_defers_model_paints_until_release() {
    let mut app = docked_app();
    let now = Instant::now();
    let view = app.windows[&WID]
        .dock
        .view
        .expect("the default dock's view");
    let dock = app
        .retained_native_leaf_artifact(WID, view, true)
        .expect("a painted dock");
    assert!(
        dock.compiled
            .hits
            .iter()
            .any(|hit| hit.key.as_str().starts_with(crate::navigator::ROW))
    );
    let dock_width = dock.width as usize;
    let (lease, owner) = Lease::with_wake(|| {});
    let captured = image(&mut app, &lease).expect("an exact presented client");
    let receipt = lease.receipt().unwrap();
    let before = glass(&mut app);
    assert_eq!((captured.0, captured.1), before.0);
    assert_eq!(captured.2, rgba(&before.1));
    assert!(dock_width != 0 && dock_width < captured.0);
    assert!(
        captured
            .2
            .chunks_exact(4)
            .take(dock_width * captured.1)
            .any(|pixel| pixel != &captured.2[..4]),
        "the client includes painted content"
    );
    let commits = surface(&mut app).commits();

    // Real host updates continue while the presentation is owned: the list takes a
    // new title and the terminal takes B, without publishing either to the surface.
    let session = app.focused_session_id(WID).unwrap();
    assert_eq!(
        app.write_session_title_pin(session, "changed while photographing")
            .ok(),
        Some(true)
    );
    app.navigator_mark_row(session);
    app.publish_navigators(now + Duration::from_secs(2));
    let tree = app.navigator_tree().unwrap();
    assert_eq!(
        tree.entries[tree.index_of(session).unwrap()].row.title,
        "changed while photographing"
    );
    feed(&app, b"deferred B");
    app.redraw_window(WID);
    assert_eq!(surface(&mut app).commits(), commits);
    assert_eq!(glass(&mut app), before);
    assert_eq!(app.windows[&WID].capture_present_serial, receipt.serial);
    assert!(
        !app.windows[&WID].redraw_pending,
        "the lease carries the debt without a due frame-cap timer"
    );
    {
        let mut state = app.windows.get_mut(&WID).unwrap();
        state.redraw_pending = true;
        let due = state.last_present_at.unwrap() + Duration::from_secs(10);
        assert!(!state.service_presentation_clocks(Duration::from_millis(1), due));
        assert!(
            !state.redraw_pending,
            "new content cannot re-arm a held frame's expired clock"
        );
    }

    let (other, other_owner) = Lease::with_wake(|| {});
    assert!(
        image(&mut app, &other).is_err(),
        "another capture cannot replace the held client"
    );
    drop(other_owner);
    app.release_presentations();
    assert!(
        app.presentation_blocked(WID),
        "another owner's release does not release this window"
    );

    assert_eq!(owner.finish(), Ok(receipt));
    app.release_presentations();
    assert!(!app.presentation_blocked(WID));
    assert!(
        app.windows[&WID].redraw_pending,
        "release re-arms the pending paint"
    );
    app.redraw_window(WID);
    let after = glass(&mut app);
    assert!(surface(&mut app).commits() > commits);
    assert_ne!(after, before);
    let dock_changed = before
        .1
        .chunks_exact(before.0.0)
        .zip(after.1.chunks_exact(after.0.0))
        .any(|(old, new)| old[..dock_width] != new[..dock_width]);
    assert!(
        dock_changed,
        "the Navigator's deferred title really paints on release"
    );

    let (next, next_owner) = Lease::with_wake(|| {});
    image(&mut app, &next).unwrap();
    let next_receipt = next.receipt().unwrap();
    app.release_presentations(); // A late wake from the previous controller.
    assert!(app.presentation_blocked(WID));
    assert_eq!(next.receipt(), Ok(next_receipt));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owner = next_owner;
        panic!("the photo's controller unwinds");
    }));
    assert!(unwound.is_err());
    app.release_presentations();
    assert!(!app.presentation_blocked(WID));
    app.redraw_window(WID);
    assert!(app.windows[&WID].capture_present_serial > next_receipt.serial);
}

#[test]
fn resize_surface_replacement_and_window_end_invalidate_a_presentation_receipt() {
    for change in ["resize", "replace", "close"] {
        let mut app = docked_app();
        let (lease, owner) = Lease::with_wake(|| {});
        image(&mut app, &lease).unwrap();
        let receipt = lease.receipt().unwrap();
        let before = glass(&mut app);
        let session = app.focused_session_id(WID).unwrap();
        assert_eq!(
            app.write_session_title_pin(session, "resize debt B").ok(),
            Some(true)
        );
        app.navigator_mark_row(session);
        app.publish_navigators(Instant::now() + Duration::from_secs(2));
        let tree = app.navigator_tree().unwrap();
        assert_eq!(
            tree.entries[tree.index_of(session).unwrap()].row.title,
            "resize debt B"
        );
        feed(&app, b"deferred resize B");
        app.redraw_window(WID);
        assert_eq!(glass(&mut app), before);
        let asks = app.windows[&WID].redraw_requests_for_test.get();
        match change {
            "resize" => app.windows.get_mut(&WID).unwrap().set_window_pixels(
                aterm_window::dpi::PhysicalSize::new(receipt.width + 1, receipt.height),
            ),
            "replace" => {
                let mut replacement = crate::present::CpuSurface::Scripted(Default::default());
                replacement
                    .resize(
                        std::num::NonZeroU32::new(receipt.width).unwrap(),
                        std::num::NonZeroU32::new(receipt.height).unwrap(),
                    )
                    .unwrap();
                assert_eq!(
                    replacement.scripted().unwrap().size(),
                    (receipt.width as usize, receipt.height as usize)
                );
                app.windows
                    .get_mut(&WID)
                    .unwrap()
                    .replace_present(Some(PresentTarget::Cpu {
                        surface: replacement,
                    }));
                assert_ne!(
                    app.windows[&WID].presentation_surface, receipt.surface,
                    "same-size recreation is a different surface"
                );
            }
            "close" => {
                app.windows.remove(&WID);
            }
            _ => unreachable!(),
        }
        assert!(lease.receipt().is_err(), "{change}");
        if change != "close" {
            let mut state = app.windows.get_mut(&WID).unwrap();
            assert!(
                state.redraw_pending,
                "{change}: invalidation transfers the held debt"
            );
            assert!(
                state.redraw_requests_for_test.get() > asks,
                "{change}: the native redraw was requested before the owner ended"
            );
            let due = state.last_present_at.unwrap() + Duration::from_secs(10);
            assert!(state.service_presentation_clocks(Duration::from_millis(1), due));
            drop(state);
            app.redraw_window(WID);
            assert!(app.windows[&WID].capture_present_serial > receipt.serial);
            assert_ne!(
                glass(&mut app),
                before,
                "{change}: pending content paints while the owner lives"
            );
        }
        assert!(
            owner.finish().is_err(),
            "an invalidated photograph cannot pass: {change}"
        );
        app.release_presentations();
        assert!(!app.presentation_blocked(WID));
    }
}

#[test]
fn an_unowned_or_failed_capture_never_installs_a_presentation_hold() {
    let mut app = app();
    let (lease, owner) = Lease::with_wake(|| {});
    let commits = surface(&mut app).commits();
    drop(owner);
    assert!(image(&mut app, &lease).is_err());
    assert_eq!(
        surface(&mut app).commits(),
        commits,
        "cancelled admission starts no capture"
    );
    assert!(!app.presentation_blocked(WID));
    let (lease, owner) = Lease::with_wake(|| {});
    for _ in 0..crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT {
        surface(&mut app).fail_next(crate::present::scripted::ScriptedOutcome::FailCommit);
    }
    assert!(image(&mut app, &lease).is_err());
    assert!(owner.finish().is_err());
    assert!(!app.presentation_blocked(WID));
    let before = glass(&mut app);
    let serial = app.windows[&WID].capture_present_serial;
    feed(&app, b"after the failed capture");
    let session = app.focused_session_id(WID).unwrap();
    app.on_output_wake(session, Instant::now());
    let interval = app.frame_interval;
    let due = {
        let mut state = app.windows.get_mut(&WID).unwrap();
        // Output admission sets this after its native-window guard. The scripted
        // window has no OS handle, but the same content and retry clocks apply.
        assert!(state.os_window.is_none());
        state.content_pending = true;
        let retry = state
            .present_retry
            .deadline
            .expect("the last failed capture owes an autonomous retry");
        assert!(!state.present_retry.parked);
        let floor = state.content_pace_floor(interval);
        let due = retry.max(state.last_present_at.unwrap() + floor);
        assert!(state.service_presentation_clocks(floor, due));
        assert!(state.present_retry.present_attempt_allowed());
        due
    };
    app.redraw_window_at(WID, due);
    assert!(surface(&mut app).commits() > commits);
    assert!(app.windows[&WID].capture_present_serial > serial);
    let after = glass(&mut app);
    assert_eq!(after.0, before.0);
    assert_ne!(
        after.1, before.1,
        "the newly received terminal text reaches glass"
    );
}

#[test]
fn a_controller_end_releases_once_and_cannot_install_after_it_ended() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&wakes);
    let (lease, owner) = Lease::with_wake(move || {
        count.fetch_add(1, Ordering::SeqCst);
    });
    let receipt = Receipt {
        window: 1,
        serial: 2,
        surface: 3,
        width: 4,
        height: 5,
    };
    let held = lease.pin(receipt).unwrap();
    lease.confirm(receipt).unwrap();
    assert!(held.blocks(receipt));
    assert_eq!(owner.finish(), Ok(receipt));
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert!(!held.blocks(receipt));
    assert!(lease.pin(receipt).is_err());
    drop(held);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

struct ControlledSubmit {
    entered: std::sync::mpsc::Sender<()>,
    answer: std::sync::mpsc::Receiver<Result<(), String>>,
    retired: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for ControlledSubmit {
    fn drop(&mut self) {
        self.retired.store(true, Ordering::SeqCst);
    }
}

impl crate::present::SubmitFence for ControlledSubmit {
    fn wait(self: Box<Self>, stopped: &(dyn Fn() -> bool + Send + Sync)) -> Result<(), String> {
        self.entered
            .send(())
            .expect("the controller watches admission");
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if stopped() {
                return Err("the native confirmation was cancelled".into());
            }
            match self.answer.recv_timeout(Duration::from_millis(10)) {
                Ok(answer) => {
                    if stopped() {
                        return Err("the native confirmation ended before acknowledgment".into());
                    }
                    return answer;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(error) => {
                    return Err(format!(
                        "the controlled native acknowledgment failed: {error}"
                    ));
                }
            }
        }
    }
}

#[test]
fn native_confirmation_precedes_publication_and_keeps_dispatch_live() {
    for end in [
        "confirmed",
        "refused",
        "controller-ended",
        "cancelled",
        "resized",
        "replaced",
    ] {
        let mut app = docked_app();
        let dir = aterm_tempfile::TempDir::new().expect("a private capture directory");
        crate::control_auth::ensure_private_dir(dir.path()).unwrap();
        let target = crate::control_auth::ConfinedImage::for_test(dir.path(), "leased.png");
        let path = target.display_path();
        let (entered, admitted) = std::sync::mpsc::channel();
        let (answer, native) = std::sync::mpsc::channel();
        let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        surface(&mut app).set_capture_fence(Box::new(ControlledSubmit {
            entered,
            answer: native,
            retired: Arc::clone(&retired),
        }));
        let (lease, owner) = Lease::with_wake(|| {});
        let mut owner = Some(owner);
        let cancel = crate::control::CaptureCancellation::new();
        let (reply, result) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target,
            clean: false,
            session: None,
            want_bytes: false,
            want_metadata: false,
            whole_client: true,
            presentation: Some(lease.clone()),
            frame_metadata: Arc::new(std::sync::OnceLock::new()),
            cancel: cancel.clone(),
            reply,
        });
        admitted
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("native wait admitted off the UI thread");
        let pinned = glass(&mut app);
        let serial = app.windows[&WID].capture_present_serial;
        let commits = surface(&mut app).commits();
        assert!(lease.receipt().is_err(), "{end}: no confirmed receipt yet");
        assert!(matches!(
            result.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(
            !path.exists(),
            "{end}: no PNG before the native acknowledgment"
        );
        feed(&app, b"the event loop still accepts B");
        app.redraw_window(WID);
        assert_eq!(surface(&mut app).commits(), commits, "{end}");
        assert_eq!(
            glass(&mut app),
            pinned,
            "{end}: exact submitted client stays pinned"
        );
        assert_eq!(app.windows[&WID].capture_present_serial, serial);
        match end {
            "confirmed" => answer.send(Ok(())).unwrap(),
            "refused" => answer
                .send(Err("native display refused the submit".into()))
                .unwrap(),
            "controller-ended" => drop(owner.take()),
            "cancelled" => {
                assert!(cancel.cancel());
            }
            "resized" => {
                let mut size = app.windows[&WID].win_px.unwrap();
                size.width += 1;
                app.windows.get_mut(&WID).unwrap().set_window_pixels(size);
            }
            "replaced" => app.windows.get_mut(&WID).unwrap().replace_present(None),
            _ => unreachable!(),
        }
        let result = result
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the native owner retires before answering");
        assert!(
            retired.load(Ordering::SeqCst),
            "{end}: native fence retired before reply"
        );
        if end == "confirmed" {
            let retained = result.expect("confirmed submission publishes");
            assert!(retained.value.0 > 0 && retained.value.1 > 0);
            assert!(path.exists());
            let encoded = std::fs::read(&path).unwrap();
            let (pixels, width, height) = aterm_render::decode_png_rgba8(&encoded).unwrap();
            assert_eq!((width, height), pinned.0);
            assert_eq!(pixels, rgba(&pinned.1));
            let receipt = lease.receipt().unwrap();
            assert_eq!(owner.take().unwrap().finish(), Ok(receipt));
        } else {
            assert!(result.is_err(), "{end}");
            assert!(
                !path.exists(),
                "{end}: refused submission cannot publish pixels"
            );
            assert!(lease.receipt().is_err(), "{end}");
        }
        app.release_presentations();
        assert!(
            !app.presentation_blocked(WID),
            "{end}: pending paint debt is released"
        );
    }
}

#[test]
fn a_submitted_frame_cannot_finish_or_outlive_its_confirmation() {
    let receipt = Receipt {
        window: 0,
        serial: 1,
        surface: 1,
        width: 2,
        height: 2,
    };
    let (lease, owner) = Lease::with_wake(|| {});
    let held = lease.pin(receipt).unwrap();
    assert!(held.blocks(receipt));
    assert!(lease.receipt().is_err());
    assert!(
        lease
            .confirm(Receipt {
                serial: receipt.serial + 1,
                ..receipt
            })
            .is_err()
    );
    assert!(
        held.blocks(receipt),
        "an unrelated acknowledgment cannot replace the pin"
    );
    assert!(
        owner.finish().is_err(),
        "an unconfirmed submit is never a material receipt"
    );
    assert!(lease.confirm(receipt).is_err());

    let mut app = app();
    let (lease, owner) = Lease::with_wake(|| {});
    let state = &app.windows[&WID];
    let receipt = state.presentation_receipt(WID).unwrap();
    let confirmation = app
        .install_presentation(
            WID,
            &lease,
            receipt.serial,
            receipt.width as usize,
            receipt.height as usize,
        )
        .unwrap();
    drop(confirmation);
    assert!(lease.receipt().is_err());
    assert!(
        !app.presentation_blocked(WID),
        "a dropped queued confirmation returns its paint debt"
    );
    assert!(owner.finish().is_err());
}

#[test]
fn a_full_encode_queue_retires_the_native_confirmation_before_refusing() {
    let mut app = app();
    let (tx, _pending) = std::sync::mpsc::sync_channel(0);
    app.encode_tx = Some(tx);
    let (entered, _admitted) = std::sync::mpsc::channel();
    let (_answer, native) = std::sync::mpsc::channel();
    let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    surface(&mut app).set_capture_fence(Box::new(ControlledSubmit {
        entered,
        answer: native,
        retired: Arc::clone(&retired),
    }));
    let (lease, owner) = Lease::with_wake(|| {});
    let error = image(&mut app, &lease).expect_err("a full queue refuses the actual image");
    assert!(error.contains("encode queue full"), "{error}");
    assert!(
        retired.load(Ordering::SeqCst),
        "native custody ended before refusal"
    );
    assert!(lease.receipt().is_err());
    assert!(!app.presentation_blocked(WID));
    assert!(owner.finish().is_err());
}
