// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GDI presenter on a live Windows desktop (LW.3's scripted CpuSurface
//! tests, docs/AUDIT-third-party-2026-10-15.md): a window of the test's own, a
//! script of resizes, paints and presents, and after each present the
//! window's pixels read back from its DC (`BitBlt` into a bitmap, then
//! `GetDIBits`) and held to what the script put on it. user32, gdi32 and
//! kernel32 are declared here, not taken from aterm-win32, so the readback is
//! a witness independent of the declarations the presenter calls.
//!
//! Session 0 (sshd's logons) has no display, so there the tests end COULD NOT
//! RUN; the Windows lane runs them in `lane`'s desktop session
//! (docs/WINDOWS-RUNTIME-LANE.md, `exec --desktop`).

use std::num::NonZeroU32;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{GdiSurface, Win32PresentError};
use crate::present::{CpuFrameBuffer as _, DamageRect};

/// user32's, gdi32's and kernel32's own declarations (winuser.h, wingdi.h,
/// processthreadsapi.h), handles as `isize` as the crate's other leaf
/// declarations take them.
#[allow(non_snake_case, clippy::upper_case_acronyms)]
mod witness {
    use std::ffi::c_void;

    pub type HWND = isize;

    /// `RECT`.
    #[repr(C)]
    #[derive(Default, Clone, Copy, Debug)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    /// `POINT`.
    #[repr(C)]
    pub struct POINT {
        pub x: i32,
        pub y: i32,
    }

    /// `MSG`: pumped, never inspected.
    #[repr(C)]
    pub struct MSG {
        pub hwnd: HWND,
        pub message: u32,
        pub wParam: usize,
        pub lParam: isize,
        pub time: u32,
        pub pt: POINT,
    }

    /// `WNDCLASSW`.
    #[repr(C)]
    pub struct WNDCLASSW {
        pub style: u32,
        pub lpfnWndProc: unsafe extern "system" fn(HWND, u32, usize, isize) -> isize,
        pub cbClsExtra: i32,
        pub cbWndExtra: i32,
        pub hInstance: isize,
        pub hIcon: isize,
        pub hCursor: isize,
        pub hbrBackground: isize,
        pub lpszMenuName: *const u16,
        pub lpszClassName: *const u16,
    }

    /// `BITMAPINFOHEADER`: for a 32-bit `BI_RGB` readback the colour table is
    /// empty, so the header alone is the `BITMAPINFO`.
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct BitmapInfoHeader {
        pub size: u32,
        pub width: i32,
        pub height: i32,
        pub planes: u16,
        pub bit_count: u16,
        pub compression: u32,
        pub size_image: u32,
        pub x_ppm: i32,
        pub y_ppm: i32,
        pub clr_used: u32,
        pub clr_important: u32,
    }

    pub const WS_POPUP: u32 = 0x8000_0000;
    pub const WS_VISIBLE: u32 = 0x1000_0000;
    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
    pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_NOACTIVATE: u32 = 0x0010;
    pub const PM_REMOVE: u32 = 0x1;
    pub const SRCCOPY: u32 = 0x00CC_0020;
    pub const DIB_RGB_COLORS: u32 = 0;
    pub const GR_GDIOBJECTS: u32 = 0;
    pub const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
    /// `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2`, aterm's own.
    pub const PER_MONITOR_AWARE_V2: isize = -4;

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
        pub fn CreateWindowExW(
            dwExStyle: u32,
            lpClassName: *const u16,
            lpWindowName: *const u16,
            dwStyle: u32,
            X: i32,
            Y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: HWND,
            hMenu: isize,
            hInstance: isize,
            lpParam: *mut c_void,
        ) -> HWND;
        pub fn DestroyWindow(hWnd: HWND) -> i32;
        pub fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: usize, lParam: isize) -> isize;
        pub fn PeekMessageW(
            lpMsg: *mut MSG,
            hWnd: HWND,
            wMsgFilterMin: u32,
            wMsgFilterMax: u32,
            wRemoveMsg: u32,
        ) -> i32;
        pub fn TranslateMessage(lpMsg: *const MSG) -> i32;
        pub fn DispatchMessageW(lpMsg: *const MSG) -> isize;
        pub fn SetWindowPos(
            hwnd: HWND,
            after: HWND,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        pub fn GetClientRect(hwnd: HWND, rect: *mut Rect) -> i32;
        pub fn GetDC(hwnd: isize) -> isize;
        pub fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
        pub fn SetThreadDpiAwarenessContext(context: isize) -> isize;
        pub fn GetGuiResources(process: isize, flags: u32) -> u32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        pub fn CreateCompatibleDC(hdc: isize) -> isize;
        pub fn CreateCompatibleBitmap(hdc: isize, w: i32, h: i32) -> isize;
        pub fn SelectObject(hdc: isize, obj: isize) -> isize;
        pub fn DeleteObject(obj: isize) -> i32;
        pub fn DeleteDC(hdc: isize) -> i32;
        pub fn BitBlt(
            hdc: isize,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            src: isize,
            x1: i32,
            y1: i32,
            rop: u32,
        ) -> i32;
        pub fn GetDIBits(
            hdc: isize,
            hbmp: isize,
            start: u32,
            lines: u32,
            bits: *mut c_void,
            bmi: *mut BitmapInfoHeader,
            usage: u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetModuleHandleW(lpModuleName: *const u16) -> isize;
        pub fn GetLastError() -> u32;
        pub fn GetCurrentProcess() -> isize;
        pub fn GetCurrentProcessId() -> u32;
        pub fn ProcessIdToSessionId(process: u32, session: *mut u32) -> i32;
    }
}

use witness as w;

/// End the test COULD NOT RUN in session 0, which draws nowhere a readback
/// sees.
fn need_a_desktop() {
    let mut session = 0;
    // SAFETY: this process's id and an out pointer.
    let ok = unsafe { w::ProcessIdToSessionId(w::GetCurrentProcessId(), &mut session) };
    assert_ne!(ok, 0, "ProcessIdToSessionId");
    if session == 0 {
        aterm_uds::host::could_not_run(
            "session 0 has no display, so there is no window to read back; run it in a desktop \
             session (`lab windows exec --desktop`)",
        );
    }
}

/// A top-level popup window of this thread, `width` x `height` client pixels
/// (a popup has no frame), topmost, never activated; destroyed when dropped.
/// One at a time in the process: each holds [`DESK`] while it lives.
struct TestWindow {
    hwnd: isize,
    _desk: MutexGuard<'static, ()>,
}

/// Held by the live [`TestWindow`], so the tests' windows never overlap.
static DESK: Mutex<()> = Mutex::new(());

const CLASS: &str = "aterm-present-gdi-test";

impl TestWindow {
    fn new(width: i32, height: i32) -> Self {
        need_a_desktop();
        let desk = DESK.lock().unwrap_or_else(PoisonError::into_inner);
        let class: Vec<u16> = CLASS.encode_utf16().chain([0]).collect();
        // SAFETY: plain calls with live arguments; the class names
        // `DefWindowProcW`, which handles every message (with no background
        // brush, painting nothing).
        unsafe {
            w::SetThreadDpiAwarenessContext(w::PER_MONITOR_AWARE_V2);
            let instance = w::GetModuleHandleW(std::ptr::null());
            let wc = w::WNDCLASSW {
                style: 0,
                lpfnWndProc: w::DefWindowProcW,
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: 0,
                hCursor: 0,
                hbrBackground: 0,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            if w::RegisterClassW(&wc) == 0 {
                assert_eq!(
                    w::GetLastError(),
                    w::ERROR_CLASS_ALREADY_EXISTS,
                    "RegisterClassW"
                );
            }
            let hwnd = w::CreateWindowExW(
                w::WS_EX_TOPMOST | w::WS_EX_TOOLWINDOW | w::WS_EX_NOACTIVATE,
                class.as_ptr(),
                class.as_ptr(),
                w::WS_POPUP | w::WS_VISIBLE,
                64,
                64,
                width,
                height,
                0,
                0,
                instance,
                std::ptr::null_mut(),
            );
            assert_ne!(hwnd, 0, "CreateWindowExW: {}", w::GetLastError());
            let window = Self { hwnd, _desk: desk };
            window.pump();
            assert_eq!(window.client(), (width, height), "the client area");
            window
        }
    }

    /// Its handle, as aterm-win32 types it.
    fn hwnd(&self) -> aterm_win32::Win32::Foundation::HWND {
        std::ptr::without_provenance_mut(self.hwnd as usize)
    }

    /// Dispatch whatever this thread's queue holds.
    fn pump(&self) {
        let mut msg = w::MSG {
            hwnd: 0,
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: w::POINT { x: 0, y: 0 },
        };
        // SAFETY: `msg` is written by PeekMessageW before it is dispatched.
        unsafe {
            while w::PeekMessageW(&mut msg, 0, 0, 0, w::PM_REMOVE) != 0 {
                w::TranslateMessage(&msg);
                w::DispatchMessageW(&msg);
            }
        }
    }

    /// The window made `width` x `height`.
    fn resize(&self, width: i32, height: i32) {
        self.resize_unpumped(width, height);
        self.pump();
    }

    /// The window made `width` x `height`, and nothing it queued dispatched:
    /// what it gained stays in its update region, as it is when aterm
    /// presents in the `Resized` the change brings, before any `WM_PAINT`.
    fn resize_unpumped(&self, width: i32, height: i32) {
        // SAFETY: this thread's window.
        let ok = unsafe {
            w::SetWindowPos(
                self.hwnd,
                0,
                64,
                64,
                width,
                height,
                w::SWP_NOZORDER | w::SWP_NOACTIVATE,
            )
        };
        assert_ne!(ok, 0, "SetWindowPos");
        assert_eq!(self.client(), (width, height), "the client area");
    }

    fn client(&self) -> (i32, i32) {
        let mut r = w::Rect::default();
        // SAFETY: this thread's window and an out rectangle.
        assert_ne!(unsafe { w::GetClientRect(self.hwnd, &mut r) }, 0);
        (r.right - r.left, r.bottom - r.top)
    }

    /// The client area's pixels as `0x00RRGGBB` words, row 0 first: copied
    /// from the window's DC into a bitmap, then read by `GetDIBits`.
    fn readback(&self) -> Vec<u32> {
        self.pump();
        let (width, height) = self.client();
        let mut out = vec![0u32; (width * height) as usize];
        // SAFETY: the window's DC, a bitmap and a DC made, used and freed
        // here; `out` holds `height` rows of `width` 32-bit words.
        unsafe {
            let window_dc = w::GetDC(self.hwnd);
            assert_ne!(window_dc, 0, "GetDC");
            let dc = w::CreateCompatibleDC(window_dc);
            let bitmap = w::CreateCompatibleBitmap(window_dc, width, height);
            assert!(dc != 0 && bitmap != 0, "a readback bitmap");
            let held = w::SelectObject(dc, bitmap);
            let copied = w::BitBlt(dc, 0, 0, width, height, window_dc, 0, 0, w::SRCCOPY);
            w::SelectObject(dc, held);
            let mut info = w::BitmapInfoHeader {
                size: size_of::<w::BitmapInfoHeader>() as u32,
                width,
                height: -height,
                planes: 1,
                bit_count: 32,
                ..w::BitmapInfoHeader::default()
            };
            let lines = w::GetDIBits(
                dc,
                bitmap,
                0,
                height as u32,
                out.as_mut_ptr().cast(),
                &mut info,
                w::DIB_RGB_COLORS,
            );
            w::DeleteObject(bitmap);
            w::DeleteDC(dc);
            w::ReleaseDC(self.hwnd, window_dc);
            assert_ne!(copied, 0, "BitBlt from the window");
            assert_eq!(lines, height, "GetDIBits's scanlines");
        }
        // BI_RGB's fourth byte is not colour.
        for px in &mut out {
            *px &= 0x00ff_ffff;
        }
        out
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: this thread's window.
        unsafe {
            w::DestroyWindow(self.hwnd);
        }
    }
}

/// This process's GDI objects.
fn gdi_objects() -> u32 {
    // SAFETY: the current process's pseudo-handle.
    unsafe { w::GetGuiResources(w::GetCurrentProcess(), w::GR_GDIOBJECTS) }
}

/// A word per pixel that differs in every channel from its neighbours' and
/// from another `seed`'s.
fn pattern(seed: u32, x: u32, y: u32) -> u32 {
    let r = (x * 31 + y * 17 + seed * 101) & 0xff;
    let g = (x * 7 + y * 3 + seed * 13) & 0xff;
    let b = (y * 11 + x * 5 + seed * 29) & 0xff;
    (r << 16) | (g << 8) | b
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> DamageRect {
    DamageRect {
        x,
        y,
        width: NonZeroU32::new(width).unwrap_or_else(|| unreachable!()),
        height: NonZeroU32::new(height).unwrap_or_else(|| unreachable!()),
    }
}

/// What a present asks of the surface.
#[derive(Clone, Debug)]
enum Commit {
    /// `present`.
    Whole,
    /// `present_with_damage`.
    Damage(Vec<DamageRect>),
}

/// One step of the script.
#[derive(Clone, Debug)]
enum Step {
    /// Size the surface (and the window, when it fits a window).
    Resize(u32, u32),
    /// Acquire, paint `seed`'s pattern into each of `paint` (all of it when
    /// empty), and commit.
    Frame {
        paint: Vec<DamageRect>,
        seed: u32,
        commit: Commit,
    },
}

/// What one step answered: the age the acquire saw (`None` for a resize),
/// whether it succeeded, and the window's pixels after it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answer {
    age: Option<u8>,
    ok: bool,
    glass: Vec<u32>,
}

/// A CPU surface the script drives. softbuffer 0.4.8's was the other until
/// it left at LW.3; what it answered is recorded in
/// docs/measured/windows-lw3-2026-10-21.md.
trait Glass {
    fn resize(&mut self, width: u32, height: u32) -> bool;
    /// Acquire, `paint` the buffer, commit: the age seen and whether the
    /// commit succeeded.
    fn frame(&mut self, paint: &mut dyn FnMut(&mut [u32]), commit: &Commit) -> (u8, bool);
}

impl Glass for GdiSurface {
    fn resize(&mut self, width: u32, height: u32) -> bool {
        match (NonZeroU32::new(width), NonZeroU32::new(height)) {
            (Some(w), Some(h)) => GdiSurface::resize(self, w, h).is_ok(),
            _ => false,
        }
    }

    fn frame(&mut self, paint: &mut dyn FnMut(&mut [u32]), commit: &Commit) -> (u8, bool) {
        let mut buffer = self.buffer_mut().expect("a sized surface");
        let age = buffer.age();
        paint(&mut buffer);
        let ok = match commit {
            Commit::Whole => buffer.present(),
            Commit::Damage(rects) => buffer.present_with_damage(rects),
        };
        (age, ok.is_ok())
    }
}

/// `seed`'s pattern painted into each of `rects` of a buffer `width` wide
/// (all of it when there are none).
fn paint_into(buf: &mut [u32], width: u32, rects: &[DamageRect], seed: u32) {
    let height = u32::try_from(buf.len()).unwrap_or(u32::MAX) / width.max(1);
    let whole = [rect(0, 0, width, height)];
    let rects = if rects.is_empty() { &whole[..] } else { rects };
    for r in rects {
        for y in r.y..r.y + r.height.get() {
            for x in r.x..r.x + r.width.get() {
                buf[(y * width + x) as usize] = pattern(seed, x, y);
            }
        }
    }
}

/// Run `script` on `glass` over `window`: each step's answer.
fn run(window: &TestWindow, glass: &mut dyn Glass, script: &[Step]) -> Vec<Answer> {
    let mut width = 0;
    let mut answers = Vec::new();
    for step in script {
        let (age, ok) = match step {
            Step::Resize(w, h) => {
                let ok = glass.resize(*w, *h);
                if ok {
                    width = *w;
                    window.resize(*w as i32, *h as i32);
                }
                (None, ok)
            }
            Step::Frame {
                paint,
                seed,
                commit,
            } => {
                let (age, ok) =
                    glass.frame(&mut |buf| paint_into(buf, width, paint, *seed), commit);
                (Some(age), ok)
            }
        };
        answers.push(Answer {
            age,
            ok,
            glass: window.readback(),
        });
    }
    answers
}

const W0: u32 = 257;
const H0: u32 = 131;
const W1: u32 = 190;
const H1: u32 = 203;

/// The script: a whole present, damage that under-claims what was painted,
/// two rectangles at the far corners, a whole present of what was painted
/// before, a resize to the same size, a new size, and the two refusals (the
/// damage one with a good rectangle before its bad one).
fn script() -> Vec<Step> {
    let frame = |paint: Vec<DamageRect>, seed, commit| Step::Frame {
        paint,
        seed,
        commit,
    };
    vec![
        Step::Resize(W0, H0),
        frame(vec![], 1, Commit::Whole),
        // Painted at (200,100) too, outside the damage: it must not show.
        frame(
            vec![rect(10, 20, 50, 30), rect(200, 100, 1, 1)],
            2,
            Commit::Damage(vec![rect(10, 20, 50, 30)]),
        ),
        frame(
            vec![rect(0, 0, W0, 1), rect(W0 - 1, H0 - 1, 1, 1)],
            3,
            Commit::Damage(vec![rect(0, 0, W0, 1), rect(W0 - 1, H0 - 1, 1, 1)]),
        ),
        // Nothing painted, all of it presented: (200,100) shows now.
        frame(vec![rect(0, 0, 1, 1)], 1, Commit::Whole),
        Step::Resize(W0, H0),
        frame(
            vec![rect(5, 5, 2, 2)],
            4,
            Commit::Damage(vec![rect(5, 5, 2, 2)]),
        ),
        Step::Resize(W1, H1),
        frame(vec![], 5, Commit::Whole),
        // The second rectangle is past GDI's coordinates: refused, and
        // nothing drawn, not even the first.
        frame(
            vec![rect(0, 0, W1, H1)],
            6,
            Commit::Damage(vec![rect(0, 0, W1, H1), rect(u32::MAX, 0, 1, 1)]),
        ),
        Step::Resize(u32::MAX, 1),
    ]
}

/// What the script must answer, step by step, by its own rules: an acquire
/// after a present sees age 1 and the buffer as it was, a present shows the
/// buffer within its damage only, a damage rectangle past GDI's coordinates
/// is refused with nothing drawn, and a resize to a new size starts at age 0.
/// A step's pixels are `None` where the script does not say them: a resized
/// window before its first present shows what Windows left there.
fn expected() -> Vec<(Option<u8>, bool, Option<Vec<u32>>)> {
    let mut answers = Vec::new();
    let (mut width, mut height) = (0u32, 0u32);
    let mut buffer: Vec<u32> = Vec::new();
    let mut glass: Option<Vec<u32>> = None;
    let mut presented = false;
    for step in script() {
        match step {
            Step::Resize(w, h) => {
                let ok = i32::try_from(w).is_ok() && i32::try_from(h).is_ok();
                if ok && (w, h) != (width, height) {
                    (width, height) = (w, h);
                    buffer = vec![0; (w * h) as usize];
                    presented = false;
                    glass = None;
                }
                answers.push((None, ok, glass.clone()));
            }
            Step::Frame {
                paint,
                seed,
                commit,
            } => {
                let age = u8::from(presented);
                paint_into(&mut buffer, width, &paint, seed);
                let ok = match &commit {
                    Commit::Whole => {
                        glass = Some(buffer.clone());
                        true
                    }
                    Commit::Damage(rects) => {
                        let fits = rects.iter().all(|r| super::gdi_rect(r).is_some());
                        if let Some(glass) = glass.as_mut().filter(|_| fits) {
                            for r in rects {
                                for y in r.y..r.y + r.height.get() {
                                    for x in r.x..r.x + r.width.get() {
                                        let i = (y * width + x) as usize;
                                        glass[i] = buffer[i];
                                    }
                                }
                            }
                        }
                        fits
                    }
                };
                presented |= ok;
                answers.push((Some(age), ok, glass.clone()));
            }
        }
    }
    answers
}

/// Where `got` differs first from `want`, at the steps `known` says the
/// pixels of: the step, and its first differing pixel.
fn first_difference(got: &[Answer], want: &[Answer], known: &[bool]) -> Option<String> {
    if got.len() != want.len() {
        return Some(format!("{} steps against {}", got.len(), want.len()));
    }
    for (i, (x, y)) in got.iter().zip(want).enumerate() {
        if (x.age, x.ok) != (y.age, y.ok) {
            return Some(format!(
                "step {i}: age {:?} ok {} against age {:?} ok {}",
                x.age, x.ok, y.age, y.ok
            ));
        }
        if !known.get(i).copied().unwrap_or(true) {
            continue;
        }
        if x.glass.len() != y.glass.len() {
            return Some(format!(
                "step {i}: {} pixels against {}",
                x.glass.len(),
                y.glass.len()
            ));
        }
        if let Some((p, (u, v))) = x
            .glass
            .iter()
            .zip(&y.glass)
            .enumerate()
            .find(|(_, (u, v))| u != v)
        {
            return Some(format!(
                "step {i}: pixel {p} {u:06x} against {v:06x} ({} of {} differ)",
                x.glass.iter().zip(&y.glass).filter(|(u, v)| u != v).count(),
                x.glass.len()
            ));
        }
    }
    None
}

/// The steps whose pixels the script says.
fn known() -> Vec<bool> {
    expected().iter().map(|(_, _, g)| g.is_some()).collect()
}

/// The script's answers on the GDI presenter, each held to the script's own
/// rules ([`expected`]).
#[test]
fn the_window_shows_what_was_presented_within_its_damage() {
    // A refused later copy still ends in a flush of the earlier queued one;
    // that first copy error wins over a simultaneous flush failure.
    let failed = rect(10, 20, 50, 30);
    for (copies_ok, flush_ok) in [(true, true), (true, false), (false, true), (false, false)] {
        let calls = std::cell::RefCell::new(Vec::new());
        let result = super::flush_present_blits(
            || {
                calls.borrow_mut().push("first copy");
                calls.borrow_mut().push("second copy");
                if copies_ok {
                    Ok(())
                } else {
                    Err(Win32PresentError::Blit(failed))
                }
            },
            || {
                calls.borrow_mut().push("flush");
                flush_ok
            },
        );
        assert_eq!(calls.into_inner(), ["first copy", "second copy", "flush"]);
        match (copies_ok, flush_ok) {
            (false, _) => assert!(matches!(result, Err(Win32PresentError::Blit(r)) if r == failed)),
            (true, false) => assert!(matches!(result, Err(Win32PresentError::Flush))),
            (true, true) => assert!(result.is_ok()),
        }
    }

    let window = TestWindow::new(W0 as i32, H0 as i32);
    // SAFETY: this thread's window, which outlives the surface.
    let mut surface = unsafe { GdiSurface::new(window.hwnd()) }.expect("a GDI surface");
    assert!(matches!(
        surface.buffer_mut().err(),
        Some(Win32PresentError::Unsized)
    ));
    let got = run(&window, &mut surface, &script());
    let want: Vec<Answer> = expected()
        .into_iter()
        .map(|(age, ok, glass)| Answer {
            age,
            ok,
            glass: glass.unwrap_or_default(),
        })
        .collect();
    if let Some(at) = first_difference(&got, &want, &known()) {
        panic!("the GDI presenter against the script: {at}");
    }
    // The script is not vacuous: its damage hid a painted pixel until the
    // whole present showed it, and the refused present drew nothing.
    let at = |answer: &Answer, x: u32, y: u32| answer.glass[(y * W0 + x) as usize];
    assert_eq!(at(&got[2], 200, 100), pattern(1, 200, 100));
    assert_eq!(at(&got[4], 200, 100), pattern(2, 200, 100));
    assert_eq!(got[9].glass, got[8].glass);
    assert!(!got[9].ok && !got[10].ok);
}

/// NEGATIVE CONTROL for the readback: a buffer painted and not presented
/// leaves the window as it was, so the readback reads the window, not the
/// buffer.
#[test]
fn a_painted_buffer_not_presented_is_not_on_the_window() {
    let window = TestWindow::new(64, 48);
    // SAFETY: this thread's window, which outlives the surface.
    let mut surface = unsafe { GdiSurface::new(window.hwnd()) }.expect("a GDI surface");
    Glass::resize(&mut surface, 64, 48);
    surface.frame(&mut |buf| paint_into(buf, 64, &[], 7), &Commit::Whole);
    let shown = window.readback();
    let mut buffer = surface.buffer_mut().expect("a sized surface");
    paint_into(&mut buffer, 64, &[], 8);
    drop(buffer);
    assert_eq!(window.readback(), shown);
    assert_ne!(shown[1], pattern(8, 1, 0));
}

/// The rows a window gains after a whole present are drawn by the next
/// present, however small its damage: Windows holds them in the window's
/// update region, and a present copies that region from the buffer before it
/// validates the client area. The window smoke's launch at 150 % kept such
/// rows white (docs/measured/windows-lw3-2026-10-21.md). NEGATIVE CONTROL:
/// with the update rectangle left out of the copy (softbuffer's protocol)
/// the gained rows are not the frame's.
#[test]
fn rows_the_window_gains_after_a_present_are_drawn_by_the_next() {
    let window = TestWindow::new(W0 as i32, H0 as i32);
    // SAFETY: this thread's window, which outlives the surface.
    let mut surface = unsafe { GdiSurface::new(window.hwnd()) }.expect("a GDI surface");
    // The buffer at the height the window is about to have, presented whole
    // while the window is shorter: the copy stops at the window's last row.
    assert!(Glass::resize(&mut surface, W0, H1));
    let (_, ok) = surface.frame(&mut |buf| paint_into(buf, W0, &[], 9), &Commit::Whole);
    assert!(ok, "the whole present");
    assert!(!surface.owes_repaint(), "a presented window owes nothing");
    window.resize_unpumped(W0 as i32, H1 as i32);
    // What aterm's `RedrawRequested` reads to present an unchanged frame.
    assert!(surface.owes_repaint(), "the gained rows are owed");
    // Nothing new: one pixel of damage, as aterm presents a frame equal to
    // the last.
    let (age, ok) = surface.frame(&mut |_| {}, &Commit::Damage(vec![rect(0, 0, 1, 1)]));
    assert_eq!((age, ok), (1, true), "the one-pixel present");
    assert!(!surface.owes_repaint(), "the present paid what was owed");
    let glass = window.readback();
    let want: Vec<u32> = (0..H1)
        .flat_map(|y| (0..W0).map(move |x| pattern(9, x, y)))
        .collect();
    let differ: Vec<usize> = (0..want.len()).filter(|&i| glass[i] != want[i]).collect();
    assert!(
        differ.is_empty(),
        "{} of {} pixels are not the frame's, rows {}..={}",
        differ.len(),
        want.len(),
        differ.first().map_or(0, |i| i / W0 as usize),
        differ.last().map_or(0, |i| i / W0 as usize),
    );
}

/// A resize frees the buffer it replaces: over [`RESIZES`] resizes the
/// process's GDI objects grow by less than half of them, where keeping each
/// replaced DIB section would add one per resize (and its DC another). The
/// count is the process's, so the margin is for what other threads make
/// meanwhile; a handle's own `GetObjectType` is no witness, since GDI hands
/// a freed handle's value to the next object made (seen in the lane).
#[test]
fn a_resize_frees_the_buffer_it_replaces() {
    let window = TestWindow::new(W0 as i32, H0 as i32);
    // SAFETY: this thread's window, which outlives the surface.
    let mut surface = unsafe { GdiSurface::new(window.hwnd()) }.expect("a GDI surface");
    assert!(Glass::resize(&mut surface, W0, H0));
    let before = gdi_objects();
    for i in 1..=RESIZES {
        assert!(Glass::resize(&mut surface, W0 + i % 2, H0));
    }
    let after = gdi_objects();
    eprintln!("the GDI presenter: {RESIZES} resizes, GDI objects {before} -> {after}");
    assert!(
        after < before + RESIZES / 2,
        "{RESIZES} resizes: GDI objects {before} -> {after}"
    );
}

const RESIZES: u32 = 200;
