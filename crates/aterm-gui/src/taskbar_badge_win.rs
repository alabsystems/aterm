// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The taskbar **overlay badge**: a small orange dot on aterm's taskbar
//! button while a window the owner is not looking at has an unanswered bell.
//!
//! A bell in an unfocused window already asks for attention
//! (`request_user_attention` → `FlashWindowEx(FLASHW_TRAY | FLASHW_TIMERNOFG)`),
//! but the flash stops after a few blinks and leaves the button lit only in
//! the shell's own colour. The badge repeats that signal and stays until the
//! window comes forward — the same moment Windows ends the flash — through
//! `ITaskbarList3::SetOverlayIcon` (the `taskbar` module in `app_window.rs`),
//! with an accessible description for screen readers.
//!
//! **Which bells.** Exactly the ones that reach a window's attention request in
//! `App::on_bell` and find that window unfocused: a bell in a background tab of
//! the window being looked at flashes that window's chrome but badges nothing.
//! `bell_sound = false` silences only the beep and keeps the attention request,
//! so it keeps the badge too; no other setting turns the request off. The OS
//! switch Settings ▸ Personalization ▸ Taskbar ▸ "Show badges on taskbar apps"
//! (and small taskbar buttons on Windows 10) hide overlays, which the shell
//! applies itself.
//!
//! **One overlay per button group.** The taskbar keeps one overlay per button,
//! and windows combined into one button share it: the last overlay received
//! wins. So clearing one window's badge while another aterm window still has
//! one puts that one's back ([`Badges::take`]).
//!
//! **When it clears.** At the first frame a badged window draws focused
//! (`App::sync_taskbar_progress`, which runs on every frame of every window),
//! or when the window closes (`taskbar::forget`). Both are one `Vec` probe
//! while nothing is badged.
//!
//! **The art.** A disc in the Windows 11 attention orange (`#F7630C`, the
//! colour the taskbar flashes a button in) with a white rim that keeps it
//! apart from the app icon beneath it, anti-aliased, drawn here at the small
//! icon size for the window's DPI (16 px at 96 DPI) and cached per size. The
//! app icon itself is not used: the badge sits on top of it.
#![cfg(windows)]

use std::cell::RefCell;
use std::collections::HashMap;

use crate::WindowId;

/// What a screen reader says for the badge.
const DESCRIPTION: &str = "A tab rang the bell";
/// The disc: Windows 11's attention orange.
const FILL: [u8; 3] = [0xF7, 0x63, 0x0C];
/// The rim between the disc and the app icon under it.
const RIM: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// One overlay call for the taskbar: show the badge on, or clear it from, a
/// window's button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Show(isize),
    Clear(isize),
}

/// Which windows hold an unanswered bell, oldest first (the taskbar shows the
/// newest overlay it received).
#[derive(Default)]
pub(crate) struct Badges {
    up: Vec<isize>,
    /// Unanswered windows whose last show was refused. Keep them in `up`:
    /// a prior overlay may still need a clear, even when restoring it failed.
    retry: Vec<isize>,
}

impl Badges {
    /// A bell asked for `hwnd`'s attention. A focused window is being looked
    /// at, so it gets none. An acknowledged badge needs no second call; a
    /// refused show may be tried again by a later bell.
    pub(crate) fn ring(&mut self, hwnd: isize, focused: bool) -> Vec<Op> {
        if focused || (self.up.contains(&hwnd) && !self.retry.contains(&hwnd)) {
            return Vec::new();
        }
        // A new bell retries a refused show and makes this the newest
        // unanswered window; an acknowledged duplicate took the return above.
        self.up.retain(|&up| up != hwnd);
        self.up.push(hwnd);
        vec![Op::Show(hwnd)]
    }

    /// A frame of `hwnd`, focused or not: a badged window that draws focused
    /// has come forward, and its badge goes.
    pub(crate) fn frame(&mut self, hwnd: isize, focused: bool) -> Vec<Op> {
        if !focused || self.up.is_empty() {
            return Vec::new();
        }
        self.take(hwnd)
    }

    /// `hwnd` is closing: its badge goes with it.
    pub(crate) fn forget(&mut self, hwnd: isize) -> Vec<Op> {
        self.take(hwnd)
    }

    /// Clear `hwnd`'s badge, and put back the newest other one: with the
    /// windows combined into one button, the clear took the group's overlay.
    fn take(&mut self, hwnd: isize) -> Vec<Op> {
        let Some(at) = self.up.iter().position(|&up| up == hwnd) else {
            return Vec::new();
        };
        self.up.remove(at);
        self.retry.retain(|&retry| retry != hwnd);
        let mut ops = vec![Op::Clear(hwnd)];
        if let Some(&newest) = self.up.last() {
            ops.push(Op::Show(newest));
        }
        ops
    }
}

/// Where overlay calls go: the taskbar in the app, a recorder in the tests.
pub(crate) trait OverlaySink {
    /// Show the badge on `hwnd`'s button with `description`, or clear it
    /// (`None`). Whether the taskbar took the call.
    fn overlay(&mut self, hwnd: isize, description: Option<&str>) -> bool;
}

/// Send `ops` to `sink`, in order. A refused show stays unanswered and
/// clearable, but lets a later bell ask again. A successful show suppresses
/// duplicate bells. Other windows and the clear/show order are unchanged.
pub(crate) fn apply(ops: &[Op], sink: &mut impl OverlaySink, badges: &mut Badges) {
    for op in ops {
        let taken = match *op {
            Op::Show(hwnd) => sink.overlay(hwnd, Some(DESCRIPTION)),
            Op::Clear(hwnd) => sink.overlay(hwnd, None),
        };
        if let Op::Show(hwnd) = *op {
            badges.retry.retain(|&retry| retry != hwnd);
            if !taken && badges.up.contains(&hwnd) {
                badges.retry.push(hwnd);
            }
        }
        if !taken {
            aterm_log::debug!("taskbar overlay not taken ({op:?})");
        }
    }
}

/// `size`×`size` pixels of the badge, BGRA with straight alpha (the 32-bit
/// icon format `CreateIcon` takes): a disc of [`FILL`] inside a rim of
/// [`RIM`], centred, anti-aliased by pixel coverage, transparent outside.
pub(crate) fn dot_pixels(size: u32) -> Vec<u8> {
    let side = size as f32;
    let centre = side / 2.0;
    let outer = side * 0.42;
    let rim = (side * 0.09).max(1.5);
    let inner = outer - rim;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - centre;
            let dy = y as f32 + 0.5 - centre;
            let d = (dx * dx + dy * dy).sqrt();
            let a_outer = (outer - d + 0.5).clamp(0.0, 1.0);
            let a_inner = (inner - d + 0.5).clamp(0.0, 1.0);
            let fill_share = if a_outer > 0.0 {
                a_inner / a_outer
            } else {
                0.0
            };
            let channel = |i: usize| {
                let v = f32::from(FILL[i]) * fill_share + f32::from(RIM[i]) * (1.0 - fill_share);
                v.round() as u8
            };
            // BGRA
            out.extend_from_slice(&[
                channel(2),
                channel(1),
                channel(0),
                (a_outer * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// The small-icon size for `dpi`: 16 px at 96 DPI, scaled, held to 16..=64.
pub(crate) fn badge_size(dpi: u32) -> u32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    ((16 * dpi + 48) / 96).clamp(16, 64)
}

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateIcon(
        instance: isize,
        width: i32,
        height: i32,
        planes: u8,
        bits_per_pixel: u8,
        and_bits: *const u8,
        xor_bits: *const u8,
    ) -> isize;
    fn GetDpiForWindow(hwnd: isize) -> u32;
}

/// What the badge's sink asks of Windows: a window's DPI, a new badge icon, and
/// the taskbar's overlay call. The app's is [`Shell`]; the tests record.
pub(crate) trait TaskbarShell {
    /// `GetDpiForWindow` (0 when the window is gone).
    fn dpi(&mut self, hwnd: isize) -> u32;
    /// A new badge icon of `size` px ([`create_icon`]), or 0.
    fn create_icon(&mut self, size: u32) -> isize;
    /// `ITaskbarList3::SetOverlayIcon` (`crate::app_window::taskbar`): `icon` 0
    /// clears; `description` is NUL-terminated UTF-16, or empty with a clear.
    fn set_overlay_icon(&mut self, hwnd: isize, icon: isize, description: &[u16]) -> bool;
}

/// The real [`TaskbarShell`].
struct Shell;

impl TaskbarShell for Shell {
    fn dpi(&mut self, hwnd: isize) -> u32 {
        // SAFETY: a plain query of a top-level window's DPI; a window that is
        // gone answers 0, which `badge_size` reads as 96.
        unsafe { GetDpiForWindow(hwnd) }
    }

    fn create_icon(&mut self, size: u32) -> isize {
        create_icon(size)
    }

    fn set_overlay_icon(&mut self, hwnd: isize, icon: isize, description: &[u16]) -> bool {
        crate::app_window::taskbar::set_overlay_icon(hwnd, icon, description)
    }
}

/// A new icon of the badge at `size` px, or 0 when the system refuses one.
fn create_icon(size: u32) -> isize {
    let pixels = dot_pixels(size);
    // The 1-bit AND mask, rows padded to 16 bits: all zero, since the colour
    // bitmap's alpha decides what shows.
    let mask = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
    let side = i32::try_from(size).unwrap_or(16);
    // SAFETY: one documented user32 call. `pixels` holds exactly size*size
    // 32-bit pixels and `mask` exactly size rows of the WORD-padded 1-bit mask
    // the call reads for that size; both outlive it. A null HICON is returned
    // as 0.
    unsafe { CreateIcon(0, side, side, 1, 32, mask.as_ptr(), pixels.as_ptr()) }
}

/// The badge's icons by size, made once each on this (UI) thread and kept for
/// the process: the taskbar copies the icon it is given. An icon the system
/// refused is not kept, so the next bell asks again.
struct Taskbar<S: TaskbarShell> {
    icons: HashMap<u32, isize>,
    shell: S,
}

impl<S: TaskbarShell> Taskbar<S> {
    fn new(shell: S) -> Self {
        Self {
            icons: HashMap::new(),
            shell,
        }
    }
}

impl<S: TaskbarShell> OverlaySink for Taskbar<S> {
    fn overlay(&mut self, hwnd: isize, description: Option<&str>) -> bool {
        let Some(description) = description else {
            return self.shell.set_overlay_icon(hwnd, 0, &[]);
        };
        let size = badge_size(self.shell.dpi(hwnd));
        let icon = match self.icons.get(&size) {
            Some(&icon) => icon,
            None => {
                let icon = self.shell.create_icon(size);
                if icon == 0 {
                    return false;
                }
                self.icons.insert(size, icon);
                icon
            }
        };
        self.shell
            .set_overlay_icon(hwnd, icon, &crate::win_ffi::wide(description))
    }
}

thread_local! {
    static BADGES: RefCell<(Badges, Taskbar<Shell>)> =
        RefCell::new((Badges::default(), Taskbar::new(Shell)));
}

/// A bell asked for the attention of `window` (focused or not): badge its
/// taskbar button unless it is focused. The one call `App::on_bell` makes.
pub(crate) fn note_bell(window: &aterm_window::window::Window, focused: bool) {
    use aterm_window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Some(hwnd) = window.window_handle().ok().and_then(|h| match h.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }) else {
        return;
    };
    BADGES.with(|b| {
        let (badges, taskbar) = &mut *b.borrow_mut();
        let ops = badges.ring(hwnd, focused);
        apply(&ops, taskbar, badges);
    });
}

/// `hwnd` is closing (`taskbar::forget`).
pub(crate) fn forget(hwnd: isize) {
    BADGES.with(|b| {
        let (badges, taskbar) = &mut *b.borrow_mut();
        let ops = badges.forget(hwnd);
        apply(&ops, taskbar, badges);
    });
    crate::jumplist_recent::forget_window(hwnd);
}

impl crate::App {
    /// The rest of the per-frame taskbar seam for window `wid` (`hwnd`, never
    /// 0): its badge clears once it draws focused, and the jump list's Recent
    /// sampler gets its frame ([`crate::App::sample_recent_dirs`]).
    pub(crate) fn sync_taskbar_extras(&self, wid: WindowId, hwnd: isize) {
        let focused = self
            .windows
            .get(&wid)
            .is_some_and(|ws| crate::os_window_focused(ws));
        BADGES.with(|b| {
            let (badges, taskbar) = &mut *b.borrow_mut();
            let ops = badges.frame(hwnd, focused);
            if !ops.is_empty() {
                apply(&ops, taskbar, badges);
            }
        });
        self.sample_recent_dirs(hwnd, focused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay calls the taskbar would receive, in order.
    #[derive(Default)]
    struct Recorder(Vec<(isize, Option<String>)>);

    impl OverlaySink for Recorder {
        fn overlay(&mut self, hwnd: isize, description: Option<&str>) -> bool {
            self.0.push((hwnd, description.map(str::to_owned)));
            true
        }
    }

    const A: isize = 0x1001;
    const B: isize = 0x2002;

    /// An unfocused window's bell badges it once; coming forward clears it;
    /// frames in between, and a bell while focused, call nothing.
    #[test]
    fn a_bell_badges_an_unfocused_window_until_it_comes_forward() {
        let mut badges = Badges::default();
        assert_eq!(badges.ring(A, false), [Op::Show(A)]);
        assert_eq!(badges.ring(A, false), [], "already badged");
        assert_eq!(badges.frame(A, false), [], "still in the background");
        assert_eq!(badges.frame(B, true), [], "another window's frame");
        assert_eq!(badges.frame(A, true), [Op::Clear(A)]);
        assert_eq!(badges.frame(A, true), [], "cleared once");
        assert_eq!(
            badges.ring(A, true),
            [],
            "a focused window is being looked at"
        );
        assert_eq!(badges.frame(A, true), []);
    }

    /// Two badged windows share one button's overlay: clearing one puts the
    /// other's back; clearing the last leaves none.
    #[test]
    fn clearing_one_window_puts_another_windows_badge_back() {
        let mut badges = Badges::default();
        assert_eq!(badges.ring(A, false), [Op::Show(A)]);
        assert_eq!(badges.ring(B, false), [Op::Show(B)]);
        assert_eq!(badges.frame(B, true), [Op::Clear(B), Op::Show(A)]);
        assert_eq!(badges.frame(A, true), [Op::Clear(A)]);
        // A closing window takes its badge along and gives the group back: the
        // clear may have taken the shared overlay even with B's the newest.
        badges.ring(A, false);
        badges.ring(B, false);
        assert_eq!(badges.forget(A), [Op::Clear(A), Op::Show(B)]);
        assert_eq!(badges.forget(B), [Op::Clear(B)]);
        assert_eq!(badges.forget(B), [], "nothing left to clear");
    }

    /// Through the sink the app's taskbar implements: a show carries the
    /// accessible description, a clear carries none.
    #[test]
    fn the_sink_gets_a_described_show_and_a_bare_clear() {
        let mut badges = Badges::default();
        let mut sink = Recorder::default();
        let ops = badges.ring(A, false);
        apply(&ops, &mut sink, &mut badges);
        let ops = badges.ring(B, false);
        apply(&ops, &mut sink, &mut badges);
        let ops = badges.frame(B, true);
        apply(&ops, &mut sink, &mut badges);
        let shown = Some(DESCRIPTION.to_owned());
        assert_eq!(
            sink.0,
            [
                (A, shown.clone()),
                (B, shown.clone()),
                (B, None),
                (A, shown)
            ]
        );
        assert!(!DESCRIPTION.is_empty());
    }

    /// The Windows calls the app's sink makes, recorded in place of the shell
    /// ([`TaskbarShell`]): the DPI it reads, the icon sizes it makes, and each
    /// overlay call as `(hwnd, icon, description, NUL-terminated)`.
    #[derive(Default)]
    struct FakeShell {
        dpi: u32,
        refuse_icons: bool,
        made: Vec<u32>,
        calls: Vec<(isize, isize, Option<String>, bool)>,
    }

    impl TaskbarShell for FakeShell {
        fn dpi(&mut self, _hwnd: isize) -> u32 {
            self.dpi
        }

        fn create_icon(&mut self, size: u32) -> isize {
            if self.refuse_icons {
                return 0;
            }
            self.made.push(size);
            0x7000 + size as isize
        }

        fn set_overlay_icon(&mut self, hwnd: isize, icon: isize, description: &[u16]) -> bool {
            let terminated = description.last() == Some(&0);
            let text = description
                .split(|&unit| unit == 0)
                .next()
                .filter(|text| !text.is_empty())
                .map(String::from_utf16_lossy);
            self.calls.push((hwnd, icon, text, terminated));
            true
        }
    }

    /// The app's sink on the shell's calls: the icon for each DPI's size is
    /// made once and shared by every window at that size, a show carries the
    /// screen reader's text NUL-terminated (`SetOverlayIcon` reads a C string),
    /// and a clear passes no icon and no text.
    #[test]
    fn the_shell_gets_one_icon_per_size_and_a_described_show() {
        let mut taskbar = Taskbar::new(FakeShell {
            dpi: 96,
            ..FakeShell::default()
        });
        let mut badges = Badges::default();
        apply(&[Op::Show(A), Op::Show(B)], &mut taskbar, &mut badges);
        taskbar.shell.dpi = 144;
        apply(&[Op::Show(A), Op::Clear(A)], &mut taskbar, &mut badges);
        assert_eq!(taskbar.shell.made, [16, 24], "one icon per size");
        let shown = || Some(DESCRIPTION.to_owned());
        assert_eq!(
            taskbar.shell.calls,
            [
                (A, 0x7010, shown(), true),
                (B, 0x7010, shown(), true),
                (A, 0x7018, shown(), true),
                (A, 0, None, false),
            ]
        );
    }

    /// A failed show goes through the actual bell bookkeeping and sink: it
    /// must let the next bell for the same still-unfocused window try again.
    /// A successful show is still sent once, and focus and close clear once.
    #[test]
    fn a_refused_icon_is_asked_for_again_at_the_next_bell() {
        let mut badges = Badges::default();
        let mut taskbar = Taskbar::new(FakeShell {
            dpi: 120,
            refuse_icons: true,
            ..FakeShell::default()
        });
        let ops = badges.ring(A, false);
        apply(&ops, &mut taskbar, &mut badges);
        assert!(
            taskbar.shell.calls.is_empty(),
            "a refused icon shows nothing"
        );

        taskbar.shell.refuse_icons = false;
        let ops = badges.ring(A, false);
        apply(&ops, &mut taskbar, &mut badges);
        assert_eq!(taskbar.shell.made, [20]);
        assert_eq!(
            taskbar.shell.calls,
            [(A, 0x7014, Some(DESCRIPTION.to_owned()), true)],
            "the next bell for the same window actually reaches the shell"
        );
        let ops = badges.ring(A, false);
        apply(&ops, &mut taskbar, &mut badges);
        assert_eq!(taskbar.shell.calls.len(), 1, "a successful show stays once");
        let ops = badges.frame(A, true);
        apply(&ops, &mut taskbar, &mut badges);
        assert_eq!(taskbar.shell.calls.last(), Some(&(A, 0, None, false)));
        assert!(badges.forget(A).is_empty(), "focus already cleared it");

        // Showing the last remaining window after another closes preserves
        // both order and bookkeeping; no duplicate stale entry was added.
        let ops = badges.ring(A, false);
        apply(&ops, &mut taskbar, &mut badges);
        let ops = badges.ring(B, false);
        apply(&ops, &mut taskbar, &mut badges);
        let ops = badges.forget(B);
        assert_eq!(ops, [Op::Clear(B), Op::Show(A)]);
        apply(&ops, &mut taskbar, &mut badges);
        let calls = &taskbar.shell.calls;
        assert_eq!(
            &calls[calls.len() - 2..],
            &[
                (B, 0, None, false),
                (A, 0x7014, Some(DESCRIPTION.to_owned()), true)
            ]
        );
        let ops = badges.forget(A);
        assert_eq!(ops, [Op::Clear(A)]);
        apply(&ops, &mut taskbar, &mut badges);
        assert!(badges.forget(A).is_empty(), "cleared once");

        // A was shown successfully before B. Clearing B asks to restore A,
        // but a new DPI can require an icon the system refuses. A's earlier
        // overlay is still possibly present and must remain clearable.
        for hwnd in [A, B] {
            let ops = badges.ring(hwnd, false);
            apply(&ops, &mut taskbar, &mut badges);
        }
        taskbar.shell.dpi = 144;
        taskbar.shell.refuse_icons = true;
        let ops = badges.forget(B);
        assert_eq!(ops, [Op::Clear(B), Op::Show(A)]);
        apply(&ops, &mut taskbar, &mut badges);
        let ops = badges.frame(A, true);
        assert_eq!(
            ops,
            [Op::Clear(A)],
            "a failed restore cannot forget a prior show"
        );
        apply(&ops, &mut taskbar, &mut badges);
        assert_eq!(taskbar.shell.calls.last(), Some(&(A, 0, None, false)));
        assert!(badges.forget(A).is_empty(), "focus already cleared it");

        // A refused restore also stays retryable. After another window's
        // bell, retrying A makes A newest; clearing an older window must put
        // A back, not the window whose bell preceded that retry.
        const C: isize = 0x3003;
        taskbar.shell.refuse_icons = false;
        for hwnd in [B, A, C] {
            let ops = badges.ring(hwnd, false);
            apply(&ops, &mut taskbar, &mut badges);
        }
        taskbar.shell.dpi = 192;
        taskbar.shell.refuse_icons = true;
        let ops = badges.forget(C);
        assert_eq!(ops, [Op::Clear(C), Op::Show(A)]);
        apply(&ops, &mut taskbar, &mut badges);
        taskbar.shell.refuse_icons = false;
        let ops = badges.ring(C, false);
        apply(&ops, &mut taskbar, &mut badges);
        let ops = badges.ring(A, false);
        assert_eq!(
            ops,
            [Op::Show(A)],
            "the refused restore is retried by a bell"
        );
        apply(&ops, &mut taskbar, &mut badges);
        assert_eq!(
            taskbar.shell.calls.last(),
            Some(&(A, 0x7020, Some(DESCRIPTION.to_owned()), true))
        );
        assert!(
            badges.ring(A, false).is_empty(),
            "the retry was acknowledged"
        );
        let ops = badges.forget(B);
        assert_eq!(
            ops,
            [Op::Clear(B), Op::Show(A)],
            "the retried bell is newest"
        );
        apply(&ops, &mut taskbar, &mut badges);
    }

    /// The art: an opaque orange centre, a white rim, transparent corners,
    /// and partial alpha on the edge (anti-aliased), at every size it is drawn.
    #[test]
    fn the_dot_is_an_orange_disc_with_a_white_rim_on_a_clear_ground() {
        for size in [16u32, 20, 24, 32, 64] {
            let px = dot_pixels(size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            let at = |x: u32, y: u32| {
                let i = ((y * size + x) * 4) as usize;
                [px[i], px[i + 1], px[i + 2], px[i + 3]]
            };
            let c = size / 2;
            assert_eq!(
                at(c, c),
                [FILL[2], FILL[1], FILL[0], 255],
                "centre at {size}"
            );
            assert_eq!(at(0, 0)[3], 0, "corner at {size}");
            assert_eq!(at(size - 1, size - 1)[3], 0, "corner at {size}");
            // Walking out from the centre along a row: fill, then the white rim
            // (opaque, its blue far above the fill's 0x0C even where a 16 px
            // rim shares its pixel with the fill), then the edge's partial
            // alpha, then nothing.
            let row: Vec<[u8; 4]> = (c..size).map(|x| at(x, c)).collect();
            assert!(
                row.iter()
                    .any(|p| p[3] == 255 && p[0] >= 0x80 && p[1] >= 0xC0),
                "a white rim at {size}"
            );
            assert!(
                row.iter().any(|p| p[3] > 0 && p[3] < 255),
                "an anti-aliased edge at {size}"
            );
            assert_eq!(
                row.last().map(|p| p[3]),
                Some(0),
                "clear past the disc at {size}"
            );
        }
    }

    /// 16 px at 96 DPI, scaled with the window's DPI, never below 16 or above 64.
    #[test]
    fn the_badge_follows_the_small_icon_size_for_the_dpi() {
        assert_eq!(badge_size(96), 16);
        assert_eq!(badge_size(120), 20);
        assert_eq!(badge_size(144), 24);
        assert_eq!(badge_size(192), 32);
        assert_eq!(badge_size(0), 16, "no DPI answer reads as 96");
        assert_eq!(badge_size(48), 16);
        assert_eq!(badge_size(960), 64);
    }

    /// The icon the taskbar would get is a real 32-bit icon of the asked size
    /// (read back through GDI; no window involved).
    #[test]
    fn the_badge_icon_is_a_real_icon_of_its_size() {
        #[repr(C)]
        struct IconInfo {
            is_icon: i32,
            x_hotspot: u32,
            y_hotspot: u32,
            mask: isize,
            color: isize,
        }
        #[repr(C)]
        struct Bitmap {
            kind: i32,
            width: i32,
            height: i32,
            width_bytes: i32,
            planes: u16,
            bits_pixel: u16,
            bits: *mut core::ffi::c_void,
        }
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetIconInfo(icon: isize, info: *mut IconInfo) -> i32;
            fn DestroyIcon(icon: isize) -> i32;
        }
        #[link(name = "gdi32")]
        unsafe extern "system" {
            fn GetObjectW(object: isize, size: i32, out: *mut core::ffi::c_void) -> i32;
            fn DeleteObject(object: isize) -> i32;
        }
        for size in [16u32, 24, 32] {
            let icon = create_icon(size);
            assert_ne!(icon, 0, "CreateIcon at {size}");
            // SAFETY: `icon` is the live icon just created; GetIconInfo fills
            // `info` and hands over two bitmaps, each read by GetObjectW into a
            // BITMAP-sized buffer and then deleted, and the icon is destroyed
            // last.
            unsafe {
                let mut info: IconInfo = std::mem::zeroed();
                assert_ne!(GetIconInfo(icon, &mut info), 0);
                assert_ne!(info.is_icon, 0, "an icon, not a cursor");
                let mut bitmap: Bitmap = std::mem::zeroed();
                let got = GetObjectW(
                    info.color,
                    i32::try_from(std::mem::size_of::<Bitmap>()).unwrap(),
                    (&mut bitmap as *mut Bitmap).cast(),
                );
                assert!(got > 0);
                assert_eq!((bitmap.width, bitmap.height), (size as i32, size as i32));
                assert_eq!(bitmap.bits_pixel, 32, "the alpha channel survives");
                DeleteObject(info.color);
                DeleteObject(info.mask);
                DestroyIcon(icon);
            }
        }
    }
}
