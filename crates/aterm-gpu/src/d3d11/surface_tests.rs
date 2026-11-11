// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::d3d11::encoder::D3dFrame;
use crate::d3d11::resources::D3dBufferRole;
use crate::device_layer::{ClearColor4, FrameLoad};
use aterm_win32::Win32::UI::WindowsAndMessaging as wm;

/// kernel32's own declarations (processthreadsapi.h), independent of
/// aterm-win32's, for the session check.
mod kernel32 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetCurrentProcessId() -> u32;
        pub fn ProcessIdToSessionId(process: u32, session: *mut u32) -> i32;
    }
}

/// COULD NOT RUN in session 0, which has no display.
pub(crate) fn desktop_or_could_not_run() {
    let mut session = 0;
    // SAFETY: plain Win32 calls; `session` receives the id.
    let ok =
        unsafe { kernel32::ProcessIdToSessionId(kernel32::GetCurrentProcessId(), &mut session) };
    if ok == 0 || session == 0 {
        aterm_spec::host::could_not_run(
            "session 0 has no display; run it in a desktop session \
             (`lab windows exec --desktop`)",
        );
    }
}

/// A shown top-level window on this thread, destroyed on drop.
pub(crate) struct Window(pub(crate) HWND);

impl Window {
    pub(crate) fn new(ex_style: u32) -> Self {
        static CLASS: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
        let name: Vec<u16> = "aterm-d3d11-surface-test\0".encode_utf16().collect();
        let atom = *CLASS.get_or_init(|| {
            let class = wm::WNDCLASSEXW {
                cbSize: std::mem::size_of::<wm::WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wm::DefWindowProcW),
                lpszClassName: name.as_ptr(),
                ..Default::default()
            };
            // SAFETY: a complete class description whose name outlives
            // the call (the class copies it).
            unsafe { wm::RegisterClassExW(&class) }
        });
        assert_ne!(atom, 0, "the test window class registers");
        // SAFETY: the class registered above; no parent, menu or data.
        let hwnd = unsafe {
            wm::CreateWindowExW(
                ex_style,
                name.as_ptr(),
                name.as_ptr(),
                wm::WS_OVERLAPPEDWINDOW,
                40,
                40,
                400,
                260,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "the test window opens");
        // SAFETY: the window just made.
        unsafe { wm::ShowWindow(hwnd, wm::SW_SHOWNOACTIVATE) };
        let w = Self(hwnd);
        w.pump();
        w
    }

    /// Drain this thread's queue, so the window is shown and sized.
    pub(crate) fn pump(&self) {
        let mut msg = wm::MSG::default();
        // SAFETY: `msg` receives each message, dispatched to its window.
        while unsafe { wm::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, wm::PM_REMOVE) } != 0
        {
            // SAFETY: as above.
            unsafe {
                wm::TranslateMessage(&msg);
                wm::DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window this test made, on its thread.
        unsafe { wm::DestroyWindow(self.0) };
        self.pump();
    }
}

/// Clear buffer 0 to `rgba` in a pass, which leaves it bound to the
/// context as the render target, as a frame's blit pass does.
pub(crate) fn clear(surf: &mut D3dWindowSurface, rgba: [f64; 4]) {
    let dev = surf.dev.clone();
    let uniform = dev
        .buffer(D3dBufferRole::Uniform, 16)
        .expect("a uniform block");
    let (w, h) = surf.size();
    let back = surf.back_buffer().expect("buffer 0");
    assert_eq!(
        (back.width, back.height),
        (w, h),
        "buffer 0 has the chain's size"
    );
    let mut frame = D3dFrame::begin(&dev).expect("the context");
    {
        let [r, g, b, a] = rgba;
        let _pass = frame
            .begin_pass(
                back,
                false,
                FrameLoad::Clear(ClearColor4 { r, g, b, a }),
                None,
                (&uniform, 0),
            )
            .expect("a pass over buffer 0");
    }
    frame.submit().expect("the clear submits");
}

/// Clear buffer 0 to `rgba`, read it back, and return its texels.
fn clear_and_read(surf: &mut D3dWindowSurface, rgba: [f64; 4]) -> Vec<u8> {
    clear(surf, rgba);
    let dev = surf.dev.clone();
    let (w, h) = surf.size();
    let back = surf.back_buffer().expect("buffer 0");
    dev.read_rgba8(back, w, h, w as usize * 4)
        .expect("buffer 0 reads back")
}

/// Every texel of `texels` is `want`.
fn uniform(texels: &[u8], want: [u8; 4]) -> bool {
    texels.as_chunks::<4>().0.iter().all(|t| *t == want)
}

/// The HWND chain: flip model with the waitable, buffer 0 drawn and read
/// back exactly, shown, then resized and drawn again at the new size.
/// The control: the second clear's colour is in the buffer, not the
/// first's.
#[test]
fn a_window_swap_chain_draws_presents_and_resizes() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(0);
    // SAFETY: the window is live, on this thread, and outlives `surf`.
    let mut surf = unsafe {
        D3dWindowSurface::create(
            &dev,
            win.0,
            D3dSwapKind::Hwnd,
            (256, 128),
            TexelFormat::Rgba8Unorm,
            2,
        )
    }
    .expect("a flip-model swap chain on the window");
    assert_eq!(surf.kind(), D3dSwapKind::Hwnd);
    assert_eq!(surf.format(), TexelFormat::Rgba8Unorm);
    assert!(
        surf.wait_for_frame(1000),
        "the waitable lets the first frame start"
    );
    let red = clear_and_read(&mut surf, [1.0, 0.0, 0.2, 1.0]);
    assert!(uniform(&red, [255, 0, 51, 255]), "buffer 0 holds the clear");
    assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
    win.pump();

    surf.resize(300, 150).expect("the buffers resize");
    assert_eq!(surf.size(), (300, 150));
    assert!(
        surf.wait_for_frame(1000),
        "the waitable lets the next frame start"
    );
    let blue = clear_and_read(&mut surf, [0.0, 0.2, 1.0, 1.0]);
    assert_eq!(blue.len(), 300 * 150 * 4);
    assert!(
        uniform(&blue, [0, 51, 255, 255]),
        "the resized buffer holds the new clear"
    );
    assert!(!uniform(&blue, [255, 0, 51, 255]), "and not the first one");
    assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
    win.pump();
}

/// An HDR switch's remake on one window: an f16 chain whose frame drew
/// into buffer 0 and never presented (so the context still binds it) is
/// dropped, and an 8-bit chain is made on its window at once; then that
/// chain presents, is dropped, and an f16 one takes the window again.
/// Each new chain draws and presents. A window takes one flip-model
/// chain at a time, and D3D11 defers a released chain's destruction while
/// its context refers to it: without the drop's forced destruction,
/// `CreateSwapChainForHwnd` is refused while the old chain lives on.
#[test]
fn a_dropped_chain_leaves_its_window_to_the_next_one() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(0);
    let make = |format, what: &str| {
        // SAFETY: as above.
        unsafe { D3dWindowSurface::create(&dev, win.0, D3dSwapKind::Hwnd, (128, 64), format, 1) }
            .unwrap_or_else(|e| panic!("{what}: {e}"))
    };
    let mut f16 = make(TexelFormat::Rgba16Float, "the first f16 chain");
    assert!(f16.wait_for_frame(60_000), "the f16 chain's frame");
    clear(&mut f16, [0.5, 0.25, 1.0, 1.0]);
    drop(f16);
    let mut sdr = make(
        TexelFormat::Rgba8Unorm,
        "an 8-bit chain on the window an f16 chain drew on and never presented",
    );
    assert!(sdr.wait_for_frame(60_000), "the 8-bit chain's frame");
    let red = clear_and_read(&mut sdr, [1.0, 0.0, 0.2, 1.0]);
    assert!(uniform(&red, [255, 0, 51, 255]), "the 8-bit chain draws");
    assert_eq!(sdr.present(0), Ok(D3dPresented::Shown));
    win.pump();
    drop(sdr);
    let mut f16 = make(
        TexelFormat::Rgba16Float,
        "an f16 chain on the window the 8-bit chain presented on",
    );
    assert!(f16.wait_for_frame(60_000), "the second f16 chain's frame");
    clear(&mut f16, [0.5, 0.25, 1.0, 1.0]);
    assert_eq!(f16.present(0), Ok(D3dPresented::Shown));
    win.pump();
}

/// The composition chain behind Mica and Acrylic: premultiplied buffers
/// shown by a DirectComposition visual on a window without a redirection
/// bitmap; a translucent texel is stored as drawn.
#[test]
fn a_composition_swap_chain_holds_premultiplied_texels() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(wm::WS_EX_NOREDIRECTIONBITMAP);
    // SAFETY: as above.
    let mut surf = unsafe {
        D3dWindowSurface::create(
            &dev,
            win.0,
            D3dSwapKind::Composition,
            (200, 100),
            TexelFormat::Rgba8Unorm,
            2,
        )
    }
    .expect("a composition swap chain shown on the window");
    assert_eq!(surf.kind(), D3dSwapKind::Composition);
    assert!(surf.wait_for_frame(1000));
    let half = clear_and_read(&mut surf, [0.4, 0.0, 0.2, 0.4]);
    assert!(
        uniform(&half, [102, 0, 51, 102]),
        "a premultiplied texel is kept"
    );
    assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
    win.pump();
}

/// The EDR chain: f16 buffers that take the scRGB tag (WARP's chains
/// support it) and present; tagged for the screen only where the
/// window's output is in HDR mode, refused by a name that says scRGB
/// otherwise (the attach then makes an 8-bit chain), so never a silent
/// untagged f16 chain.
#[test]
fn an_f16_swap_chain_takes_scrgb_where_the_output_is_hdr() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(0);
    // SAFETY: as above.
    let mut surf = unsafe {
        D3dWindowSurface::create(
            &dev,
            win.0,
            D3dSwapKind::Hwnd,
            (128, 64),
            TexelFormat::Rgba16Float,
            2,
        )
    }
    .expect("an f16 swap chain");
    assert_eq!(surf.format(), TexelFormat::Rgba16Float);
    surf.tag_scrgb()
        .expect("the chain takes the scRGB colour space");
    let hdr = surf.output_hdr();
    match surf.tag_scrgb_on_hdr_output() {
        Ok(()) => assert!(hdr, "tagged for the screen only on an HDR output"),
        Err(e) => {
            assert!(!hdr, "an HDR output takes the tag: {e}");
            assert!(e.contains("scRGB"), "the refusal names scRGB: {e}");
        }
    }
    crate::stderr_line!("d3d11 surface: the window's output is HDR: {hdr}");
    assert!(surf.wait_for_frame(1000));
    let back = surf.back_buffer().expect("an f16 buffer 0");
    assert_eq!(back.texel_format(), TexelFormat::Rgba16Float);
    assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
}

/// A frame the waitable gave stays held until a present spends it: a
/// second wait with no present between answers with the held frame, and
/// the wait after a present answers with the frame that present gives
/// back. The negative control: the waitable itself has no second frame
/// to give a latency-1 chain before a present, and times out, which is
/// the wait every later frame would time out on without the hold.
#[test]
fn a_waited_frame_is_held_until_a_present_spends_it() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(0);
    // SAFETY: as above.
    let mut surf = unsafe {
        D3dWindowSurface::create(
            &dev,
            win.0,
            D3dSwapKind::Hwnd,
            (64, 64),
            TexelFormat::Rgba8Unorm,
            1,
        )
    }
    .expect("a swap chain");
    assert!(surf.wait_for_frame(60_000), "the first frame");
    assert!(
        surf.wait_for_frame(1000),
        "a frame no present spent answers the next wait"
    );
    assert!(
        !surf.take_frame_unheld_for_test(1000),
        "the control: the waitable has no second frame before a present"
    );
    clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(surf.present(0), Ok(D3dPresented::Shown));
    win.pump();
    assert!(
        surf.wait_for_frame(60_000),
        "the present gives the waitable its frame back"
    );
}

/// A minimized window's present gives the waitable its frame back, which
/// the hold's release on every present relies on. What `Present`
/// answers there is printed: in the Windows lane's desktop, on WARP, it
/// is `Shown` (DXGI does not report the minimized flip chain occluded),
/// so this does not reach the `Occluded` arm. The control: the same
/// chain, not minimized, answers `Shown` and gives the frame back.
#[test]
fn a_minimized_windows_present_gives_the_waitable_its_frame_back() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(0);
    // SAFETY: as above.
    let mut surf = unsafe {
        D3dWindowSurface::create(
            &dev,
            win.0,
            D3dSwapKind::Hwnd,
            (64, 64),
            TexelFormat::Rgba8Unorm,
            1,
        )
    }
    .expect("a swap chain");
    assert!(surf.wait_for_frame(60_000), "the first frame");
    clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(surf.present(0), Ok(D3dPresented::Shown), "the control");
    win.pump();
    assert!(
        surf.wait_for_frame(60_000),
        "the control's frame comes back"
    );
    // SAFETY: the window this test made, on its thread.
    unsafe { wm::ShowWindow(win.0, wm::SW_MINIMIZE) };
    win.pump();
    clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
    let minimized = surf.present(0);
    win.pump();
    let back = surf.wait_for_frame(60_000);
    crate::stderr_line!(
        "d3d11 surface: a minimized window's present answered {minimized:?}; \
         the waitable gave the frame back: {back}"
    );
    assert!(minimized.is_ok(), "{minimized:?}");
    assert!(back, "the minimized window's present gives its frame back");
}

/// The removal latch at the present: after the injected-loss drill the
/// present fails and the device reports itself lost. The control: the
/// same present before the drill shows the frame.
#[test]
fn a_removed_device_fails_the_present_and_latches_lost() {
    desktop_or_could_not_run();
    let dev = crate::d3d11::test_device();
    let win = Window::new(0);
    // SAFETY: as above.
    let mut surf = unsafe {
        D3dWindowSurface::create(
            &dev,
            win.0,
            D3dSwapKind::Hwnd,
            (64, 64),
            TexelFormat::Rgba8Unorm,
            1,
        )
    }
    .expect("a swap chain");
    assert!(surf.wait_for_frame(1000));
    clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(
        surf.present(1),
        Ok(D3dPresented::Shown),
        "the control shows"
    );
    assert!(!dev.shared().lost());
    dev.shared().inject_device_loss();
    let e = surf
        .present(1)
        .expect_err("a removed device's present fails");
    assert!(e.contains("removed"), "{e}");
    assert!(dev.shared().lost(), "the removal is latched");
}
