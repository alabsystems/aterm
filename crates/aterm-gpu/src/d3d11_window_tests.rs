// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shipped Windows build's window presents on real windows
//! (`d3d11_window.rs`, docs/AUDIT-third-party-2026-10-15.md GW.5's flip). A
//! window's present composes into its swap chain's buffer what the arm's
//! headless present composes into the virtual target, which
//! `frame_record_tests` holds to its CPU twin; so each window present,
//! read back by the presented-snapshot tap before the chain shows it, must
//! equal the headless present of the same input byte for byte.
//!
//! The windows need a desktop: in the Windows lane they run in `lane`'s
//! session (`lab windows exec --desktop`); in session 0 each ends COULD NOT
//! RUN.

use super::replay_fixtures::*;
use super::*;
use crate::d3d11::surface::D3dSwapKind;
use crate::d3d11::surface::tests::{Window, desktop_or_could_not_run};
use aterm_win32::Win32::UI::WindowsAndMessaging as wm;

/// A test window as the frontend hands one over: its `HWND`.
struct Target(isize);

// SAFETY: the window outlives every surface a test attaches to it.
unsafe impl aterm_types::native_window::HasNativeWindow for Target {
    fn native_window(&self) -> Option<aterm_types::native_window::NativeWindow> {
        let hwnd =
            std::ptr::NonNull::new(std::ptr::without_provenance_mut(self.0.cast_unsigned()))?;
        Some(aterm_types::native_window::NativeWindow::Win32 {
            hwnd,
            hinstance: None,
        })
    }
}

/// A renderer on the D3D11 arm (armed at construction), without the
/// wall-clock layers; COULD NOT RUN with no device.
fn renderer() -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
    assert_eq!(
        ctx.backend, "D3d11",
        "the Windows context is the D3D11 arm's"
    );
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    gpu
}

/// One window present of `input` through the presented-snapshot tap, decoded
/// as [`presented`] decodes the headless one.
fn window_presented(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    surf: &mut GpuSurface,
    input: &RenderInput,
    invert: bool,
) -> Frame {
    gpu.reset_glow_ease_for_test(win);
    gpu.presented_snapshot_begin(win, surf)
        .expect("a snapshot of the window arms");
    gpu.present_input(win, surf, input, invert, None, None)
        .unwrap_or_else(|e| panic!("the window present failed: {e:?}"));
    assert!(
        gpu.last_frame_on_d3d11(),
        "the window present drew on D3D11"
    );
    gpu.presented_snapshot_after_present(win, 1_000)
        .expect("the snapshot stamps");
    gpu.presented_snapshot_finish(win)
        .expect("the snapshot completes");
    let shot = gpu
        .presented_snapshot_take(win)
        .expect("the snapshot is taken")
        .convert()
        .expect("the snapshot converts");
    Frame {
        width: shot.w as usize,
        height: shot.h as usize,
        pixels: shot
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, a]| {
                ((255 - u32::from(a)) << 24)
                    | (u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b)
            })
            .collect(),
    }
}

/// Every fixture presented to a real window (three frames each, the second
/// and third over the resident frame), plain and inverted: byte-identical to
/// the headless present of the same input. Controls: the inversion and a
/// changed row each change the window's frame.
#[test]
fn a_window_present_draws_what_the_headless_present_draws() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    let window = Window::new(0);
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let mut surf = gpu
        .create_window_surface(Target(window.0 as isize), fw as u32, fh as u32)
        .expect("a D3D11 swap chain on the window");
    assert!(!surf.is_hdr(), "an SDR attach is 8-bit");
    let mut on_glass = WindowGpu::new();
    let mut headless = WindowGpu::new();
    let fixtures = fixtures(&gpu, true);
    assert!(!fixtures.is_empty());
    for (name, input, _) in &fixtures {
        for (frame, invert) in [(0, false), (1, false), (2, true)] {
            let want = presented(&mut gpu, &mut headless, input, invert, None, None, None);
            let got = window_presented(&mut gpu, &mut on_glass, &mut surf, input, invert);
            assert_frames_identical(&format!("{name}, present {frame}"), &want, &got);
            window.pump();
        }
    }
    let (name, input, _) = &fixtures[0];
    let plain = window_presented(&mut gpu, &mut on_glass, &mut surf, input, false);
    let inverted = window_presented(&mut gpu, &mut on_glass, &mut surf, input, true);
    assert!(
        frames_differ(name, &plain, &inverted).is_some(),
        "the inversion changes the window's frame"
    );
    // A host write after the fill: the row stamps are not trusted.
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    if let Some(cell) = changed.cells.get_mut(1).and_then(|r| r.get_mut(2)) {
        cell.ch = 'Z';
    }
    let moved = window_presented(&mut gpu, &mut on_glass, &mut surf, &changed, false);
    assert!(
        frames_differ(name, &plain, &moved).is_some(),
        "a changed row changes the window's frame"
    );
}

/// The chain follows the window's size: after `resize_surface` the next
/// present's destination is the new size, letterboxed as the headless present
/// at that size is.
#[test]
fn the_window_chain_follows_resize_surface() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    let window = Window::new(0);
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let (fw, fh) = (fw as u32, fh as u32);
    let mut surf = gpu
        .create_window_surface(Target(window.0 as isize), fw, fh)
        .expect("a D3D11 swap chain on the window");
    let mut on_glass = WindowGpu::new();
    let mut headless = WindowGpu::new();
    let (_, input, _) = fixtures(&gpu, false).swap_remove(0);
    let first = window_presented(&mut gpu, &mut on_glass, &mut surf, &input, false);
    assert_eq!((first.width, first.height), (fw as usize, fh as usize));
    let (dw, dh) = (fw + 37, fh + 11);
    gpu.resize_surface(&mut on_glass, &mut surf, dw, dh);
    let got = window_presented(&mut gpu, &mut on_glass, &mut surf, &input, false);
    assert_eq!(
        (got.width, got.height),
        (dw as usize, dh as usize),
        "the resized destination"
    );
    // The whole frame, letterboxed into the larger destination.
    let crop = PresentCrop {
        source_y: 0,
        height: fh,
    };
    let want = presented(
        &mut gpu,
        &mut headless,
        &input,
        false,
        None,
        None,
        Some((crop, (dw, dh))),
    );
    assert_frames_identical("resized window", &want, &got);
}

/// A visual instance (Mica and Acrylic) attaches a DirectComposition chain on
/// a window without a redirection bitmap and presents on it; opaque without
/// backdrop margins, so it draws what the headless present draws.
#[test]
fn a_visual_instance_presents_through_a_composition_chain() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    gpu.ctx.visual_swapchain = true;
    let window = Window::new(wm::WS_EX_NOREDIRECTIONBITMAP);
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let mut surf = gpu
        .create_window_surface(Target(window.0 as isize), fw as u32, fh as u32)
        .expect("a composition swap chain on the window");
    assert_eq!(
        surf.d3d11.as_ref().map(|dw| dw.kind()),
        Some(D3dSwapKind::Composition)
    );
    let mut on_glass = WindowGpu::new();
    let mut headless = WindowGpu::new();
    let (name, input, _) = fixtures(&gpu, false).swap_remove(0);
    let want = presented(&mut gpu, &mut headless, &input, false, None, None, None);
    let got = window_presented(&mut gpu, &mut on_glass, &mut surf, &input, false);
    assert_frames_identical(name, &want, &got);
}

/// The Mica and Acrylic retry path (aterm-gui's
/// `retry_attach_on_opaque_swapchain`) on the D3D11 arm. A visual instance's
/// first attach that DirectComposition refuses (here a window another
/// composition already targets: `CreateTargetForHwnd` answers
/// `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED`) fails by the name the
/// frontend reports, the fallback says rebuild opaque, and the renderer
/// rebuilt on a fresh context (the frontend withdraws the visual request
/// first; a test never makes one) attaches an HWND chain to the same window
/// and presents what its headless present draws. The control: the same
/// attach on a window nothing composes takes the composition chain.
#[test]
fn a_refused_composition_attach_retries_on_the_opaque_chain() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    gpu.ctx.visual_swapchain = true;
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let (fw, fh) = (fw as u32, fh as u32);
    let free = Window::new(wm::WS_EX_NOREDIRECTIONBITMAP);
    let control = gpu
        .create_window_surface(Target(free.0 as isize), fw, fh)
        .expect("a composition chain on a window nothing composes");
    assert_eq!(
        control.d3d11.as_ref().map(|dw| dw.kind()),
        Some(D3dSwapKind::Composition)
    );
    drop(control);

    let window = Window::new(wm::WS_EX_NOREDIRECTIONBITMAP);
    let dev = crate::d3d11::test_device();
    // SAFETY: the window is live, on this thread, and outlives `foreign`.
    let foreign = unsafe {
        crate::d3d11::surface::D3dWindowSurface::create(
            &dev,
            window.0,
            D3dSwapKind::Composition,
            (64, 64),
            crate::device_layer::TexelFormat::Rgba8Unorm,
            1,
        )
    }
    .expect("another composition targets the window first");
    let Err(first_error) = gpu.create_window_surface(Target(window.0 as isize), fw, fh) else {
        panic!("DirectComposition took a second target on a composed window");
    };
    assert!(
        first_error.starts_with("DirectComposition visual swapchain configure failed"),
        "{first_error}"
    );
    assert_eq!(
        crate::surface_attach_fallback(gpu.ctx.visual_swapchain, false),
        crate::SurfaceAttachFallback::RebuildOpaque
    );
    // The frontend's font worker seals the generation before any attach.
    let _ = gpu.cpu.seal_admitted_font_sources();
    let mut rebuilt = gpu
        .rebuild_on_fresh_context(18.0)
        .expect("the renderer rebuilds on a fresh context");
    assert!(
        !rebuilt.ctx.visual_swapchain,
        "the rebuilt context is opaque"
    );
    rebuilt.set_bloom(false);
    rebuilt.set_shimmer(false);
    let mut surf = rebuilt
        .create_window_surface(Target(window.0 as isize), fw, fh)
        .expect("the opaque retry attaches");
    assert_eq!(
        surf.d3d11.as_ref().map(|dw| dw.kind()),
        Some(D3dSwapKind::Hwnd)
    );
    let mut on_glass = WindowGpu::new();
    let mut headless = WindowGpu::new();
    let (name, input, _) = fixtures(&rebuilt, false).swap_remove(0);
    let want = presented(&mut rebuilt, &mut headless, &input, false, None, None, None);
    let got = window_presented(&mut rebuilt, &mut on_glass, &mut surf, &input, false);
    assert_frames_identical(name, &want, &got);
    window.pump();
    drop(surf);
    drop(foreign);
}

/// With `hdr_glow` on, the chain is f16 only where the window's output is in
/// HDR mode (the lane's WARP desktop is SDR, so 8-bit there), and either way
/// the window present draws.
#[test]
fn hdr_glow_attaches_f16_only_on_an_hdr_output() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    gpu.set_hdr_glow(true);
    let window = Window::new(0);
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let mut surf = gpu
        .create_window_surface(Target(window.0 as isize), fw as u32, fh as u32)
        .expect("a D3D11 swap chain on the window");
    let output_hdr = surf.d3d11.as_ref().is_some_and(|dw| dw.output_hdr());
    assert_eq!(surf.is_hdr(), output_hdr, "f16 exactly on an HDR output");
    crate::stderr_line!(
        "d3d11 window: output HDR {output_hdr}, chain f16 {}",
        surf.is_hdr()
    );
    let mut on_glass = WindowGpu::new();
    let (_, input, _) = fixtures(&gpu, false).swap_remove(0);
    let got = window_presented(&mut gpu, &mut on_glass, &mut surf, &input, false);
    assert_eq!((got.width, got.height), (fw, fh));
}

/// The removal latch at a window's present: after the drill's present the
/// next one fails and the renderer reports the device lost. The control: the
/// presents before it show.
#[test]
fn a_removed_device_fails_the_window_present_and_reaches_device_lost() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    let window = Window::new(0);
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let mut surf = gpu
        .create_window_surface(Target(window.0 as isize), fw as u32, fh as u32)
        .expect("a D3D11 swap chain on the window");
    let mut win = WindowGpu::new();
    let (_, input, _) = fixtures(&gpu, false).swap_remove(0);
    gpu.d3d11_inject_loss_after_for_test(2);
    for n in 0..2 {
        assert_eq!(
            gpu.present_input(&mut win, &mut surf, &input, false, None, None),
            Ok(()),
            "present {n} before the drill shows"
        );
        assert!(!gpu.device_lost());
    }
    assert!(
        gpu.present_input(&mut win, &mut surf, &input, false, None, None)
            .is_err(),
        "the drilled device's present fails"
    );
    assert!(gpu.device_lost(), "the removal reaches device_lost");
    assert!(
        gpu.device_loss_reason()
            .is_some_and(|r| r.contains("ATERM_D3D11_INJECT_LOSS")),
        "the loss names the drill: {:?}",
        gpu.device_loss_reason()
    );
}

/// A frame whose draw fails after its wait keeps the chain's frame: what such
/// a present leaves (one wait no present followed) is taken by the next
/// present's wait, which does not time out, and the presents after it show
/// as the ones before it did. The control: a frame taken from the waitable
/// past the hold, which no present will give back, makes the next present's
/// wait time out once, and that present draws and shows all the same
/// (wgpu-dx12's acquire), giving the frame back, so the present after it
/// does not time out.
#[test]
fn a_frame_that_failed_after_its_wait_does_not_stall_the_next_present() {
    desktop_or_could_not_run();
    let mut gpu = renderer();
    let window = Window::new(0);
    let (fw, fh) = gpu.frame_size(ROWS, COLS);
    let mut surf = gpu
        .create_window_surface(Target(window.0 as isize), fw as u32, fh as u32)
        .expect("a D3D11 swap chain on the window");
    let mut win = WindowGpu::new();
    let (_, input, _) = fixtures(&gpu, false).swap_remove(0);
    let timed_out = |surf: &GpuSurface| {
        surf.d3d11
            .as_ref()
            .expect("a D3D11 window")
            .timed_out_waits()
    };
    let mut present = |gpu: &mut GpuRenderer, surf: &mut GpuSurface, what: &str| {
        assert_eq!(
            gpu.present_input(&mut win, surf, &input, false, None, None),
            Ok(()),
            "{what} shows"
        );
        window.pump();
    };
    for n in 0..3 {
        present(&mut gpu, &mut surf, &format!("present {n}"));
    }
    assert_eq!(timed_out(&surf), 0, "the presents before take every frame");

    let chain = surf.d3d11.as_mut().expect("a D3D11 window").chain_mut();
    assert!(
        chain.wait_for_frame(60_000),
        "the failed frame's wait takes the chain's frame"
    );
    for n in 0..3 {
        present(
            &mut gpu,
            &mut surf,
            &format!("present {n} after the failed frame"),
        );
    }
    assert_eq!(
        timed_out(&surf),
        0,
        "the next present takes the held frame without a wait"
    );

    // The control: a frame lost past the hold.
    let chain = surf.d3d11.as_mut().expect("a D3D11 window").chain_mut();
    assert!(
        chain.take_frame_unheld_for_test(60_000),
        "the lost frame is the chain's one"
    );
    present(&mut gpu, &mut surf, "the present after the lost frame");
    assert_eq!(timed_out(&surf), 1, "its wait timed out, and it showed");
    for n in 0..3 {
        present(&mut gpu, &mut surf, &format!("present {n} after that"));
    }
    assert_eq!(
        timed_out(&surf),
        1,
        "the timed-out present gave the frame back"
    );
}
