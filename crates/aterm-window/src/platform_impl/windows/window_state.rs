// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/window_state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use crate::dpi::{PhysicalPosition, PhysicalSize, Size};
use crate::icon::Icon;
use crate::keyboard::ModifiersState;
use crate::platform_impl::platform::{event_loop, util, Fullscreen, SelectedCursor};
use crate::window::{Theme, WindowAttributes};
use std::io;
use std::sync::MutexGuard;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::InvalidateRgn;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, EnableMenuItem, GetMenu, GetSystemMenu, GetWindowLongW, SendMessageW,
    SetWindowLongW, SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, HWND_BOTTOM, HWND_NOTOPMOST,
    HWND_TOPMOST, MF_BYCOMMAND, MF_DISABLED, MF_ENABLED, SC_CLOSE, SHOW_WINDOW_CMD,
    SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOREPOSITION, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE,
    WINDOWPLACEMENT, WINDOW_EX_STYLE, WINDOW_STYLE, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_ACCEPTFILES, WS_EX_APPWINDOW, WS_EX_LAYERED,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_EX_WINDOWEDGE, WS_MAXIMIZE,
    WS_MAXIMIZEBOX, WS_MINIMIZE, WS_MINIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_SIZEBOX,
    WS_SYSMENU, WS_VISIBLE,
};

/// Contains information about states and the window that the callback is going to use.
pub(crate) struct WindowState {
    pub mouse: MouseProperties,

    /// Used by `WM_GETMINMAXINFO`.
    pub min_size: Option<Size>,
    pub max_size: Option<Size>,

    pub resize_increments: Option<Size>,

    pub window_icon: Option<Icon>,
    pub taskbar_icon: Option<Icon>,

    pub saved_window: Option<SavedWindow>,
    pub scale_factor: f64,

    pub modifiers_state: ModifiersState,
    pub fullscreen: Option<Fullscreen>,
    pub current_theme: Theme,
    pub preferred_theme: Option<Theme>,

    pub window_flags: WindowFlags,

    pub ime_state: ImeState,
    pub ime_allowed: bool,

    pub is_active: bool,
    pub is_focused: bool,

    pub redraw_requested: bool,

    pub dragging: bool,

    pub skip_taskbar: bool,
}

#[derive(Clone)]
pub struct SavedWindow {
    pub placement: WINDOWPLACEMENT,
}

#[derive(Clone)]
pub struct MouseProperties {
    pub(crate) selected_cursor: SelectedCursor,
    pub capture_count: u32,
    cursor_flags: CursorFlags,
    pub last_position: Option<PhysicalPosition<f64>>,
}

aterm_types::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CursorFlags: u8 {
        const GRABBED   = 1 << 0;
        const HIDDEN    = 1 << 1;
        const IN_WINDOW = 1 << 2;
        const LOCKED    = 1 << 3;
    }
}
aterm_types::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct WindowFlags: u32 {
        const RESIZABLE         = 1 << 0;
        const MINIMIZABLE       = 1 << 1;
        const MAXIMIZABLE       = 1 << 2;
        const CLOSABLE          = 1 << 3;
        const VISIBLE           = 1 << 4;
        const ON_TASKBAR        = 1 << 5;
        const ALWAYS_ON_TOP     = 1 << 6;
        const ALWAYS_ON_BOTTOM  = 1 << 7;
        const NO_BACK_BUFFER    = 1 << 8;
        const TRANSPARENT       = 1 << 9;
        const CHILD             = 1 << 10;
        const MAXIMIZED         = 1 << 11;
        const POPUP             = 1 << 12;

        /// Marker flag for fullscreen. Should always match `WindowState::fullscreen`, but is
        /// included here to make masking easier.
        const MARKER_EXCLUSIVE_FULLSCREEN = 1 << 13;
        const MARKER_BORDERLESS_FULLSCREEN = 1 << 14;

        /// The `WM_SIZE` event contains some parameters that can effect the state of `WindowFlags`.
        /// In most cases, it's okay to let those parameters change the state. However, when we're
        /// running the `WindowFlags::apply_diff` function, we *don't* want those parameters to
        /// effect our stored state, because the purpose of `apply_diff` is to update the actual
        /// window's state to match our stored state. This controls whether to accept those changes.
        const MARKER_RETAIN_STATE_ON_SIZE = 1 << 15;

        const MARKER_IN_SIZE_MOVE = 1 << 16;

        const MINIMIZED = 1 << 17;

        const IGNORE_CURSOR_EVENT = 1 << 18;

        /// Fully decorated window (incl. caption, border and drop shadow).
        const MARKER_DECORATIONS = 1 << 19;
        /// Drop shadow for undecorated windows.
        const MARKER_UNDECORATED_SHADOW = 1 << 20;

        const MARKER_ACTIVATE = 1 << 21;

        const CLIP_CHILDREN = 1 << 22;

        const EXCLUSIVE_FULLSCREEN_OR_MASK = WindowFlags::ALWAYS_ON_TOP.bits();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImeState {
    Disabled,
    Enabled,
    Preedit,
}

impl WindowState {
    pub(crate) fn new(
        attributes: &WindowAttributes,
        scale_factor: f64,
        current_theme: Theme,
        preferred_theme: Option<Theme>,
    ) -> WindowState {
        WindowState {
            mouse: MouseProperties {
                selected_cursor: SelectedCursor::default(),
                capture_count: 0,
                cursor_flags: CursorFlags::empty(),
                last_position: None,
            },

            min_size: attributes.min_inner_size,
            max_size: attributes.max_inner_size,

            resize_increments: attributes.resize_increments,

            window_icon: attributes.window_icon.clone(),
            taskbar_icon: None,

            saved_window: None,
            scale_factor,

            modifiers_state: ModifiersState::default(),
            fullscreen: None,
            current_theme,
            preferred_theme,
            window_flags: WindowFlags::empty(),

            ime_state: ImeState::Disabled,
            ime_allowed: false,

            is_active: false,
            is_focused: false,
            redraw_requested: false,

            dragging: false,

            skip_taskbar: false,
        }
    }

    pub fn window_flags(&self) -> WindowFlags {
        self.window_flags
    }

    pub fn set_window_flags<F>(mut this: MutexGuard<'_, Self>, window: HWND, f: F)
    where
        F: FnOnce(&mut WindowFlags),
    {
        let old_flags = this.window_flags;
        f(&mut this.window_flags);
        let new_flags = this.window_flags;

        drop(this);
        old_flags.apply_diff(window, new_flags);
    }

    pub fn set_window_flags_in_place<F>(&mut self, f: F)
    where
        F: FnOnce(&mut WindowFlags),
    {
        f(&mut self.window_flags);
    }

    pub fn has_active_focus(&self) -> bool {
        self.is_active && self.is_focused
    }

    pub fn set_active(&mut self, is_active: bool) -> bool {
        let old = self.has_active_focus();
        self.is_active = is_active;
        old != self.has_active_focus()
    }

    pub fn set_focused(&mut self, is_focused: bool) -> bool {
        let old = self.has_active_focus();
        self.is_focused = is_focused;
        old != self.has_active_focus()
    }
}

impl MouseProperties {
    pub fn cursor_flags(&self) -> CursorFlags {
        self.cursor_flags
    }

    pub fn set_cursor_flags<F>(&mut self, window: HWND, f: F) -> Result<(), io::Error>
    where
        F: FnOnce(&mut CursorFlags),
    {
        let old_flags = self.cursor_flags;
        f(&mut self.cursor_flags);
        match self.cursor_flags.refresh_os_cursor(window) {
            Ok(()) => (),
            Err(e) => {
                self.cursor_flags = old_flags;
                return Err(e);
            },
        }

        Ok(())
    }
}

impl WindowFlags {
    fn mask(mut self) -> WindowFlags {
        if self.contains(WindowFlags::MARKER_EXCLUSIVE_FULLSCREEN) {
            self |= WindowFlags::EXCLUSIVE_FULLSCREEN_OR_MASK;
        }
        self
    }

    pub fn to_window_styles(self) -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
        let mut style = WS_CAPTION | WS_BORDER | WS_CLIPSIBLINGS | WS_SYSMENU;
        let mut style_ex = WS_EX_WINDOWEDGE | WS_EX_ACCEPTFILES;

        if self.contains(WindowFlags::RESIZABLE) {
            style |= WS_SIZEBOX;
        }
        if self.contains(WindowFlags::MAXIMIZABLE) {
            style |= WS_MAXIMIZEBOX;
        }
        if self.contains(WindowFlags::MINIMIZABLE) {
            style |= WS_MINIMIZEBOX;
        }
        if self.contains(WindowFlags::VISIBLE) {
            style |= WS_VISIBLE;
        }
        if self.contains(WindowFlags::ON_TASKBAR) {
            style_ex |= WS_EX_APPWINDOW;
        }
        if self.contains(WindowFlags::ALWAYS_ON_TOP) {
            style_ex |= WS_EX_TOPMOST;
        }
        if self.contains(WindowFlags::NO_BACK_BUFFER) {
            style_ex |= WS_EX_NOREDIRECTIONBITMAP;
        }
        if self.contains(WindowFlags::CHILD) {
            style |= WS_CHILD;

            if !self.contains(WindowFlags::MARKER_DECORATIONS) {
                style &= !(WS_CAPTION | WS_BORDER);
                style_ex &= !WS_EX_WINDOWEDGE;
            }
        }
        if self.contains(WindowFlags::POPUP) {
            style |= WS_POPUP;
        }
        if self.contains(WindowFlags::MINIMIZED) {
            style |= WS_MINIMIZE;
        }
        if self.contains(WindowFlags::MAXIMIZED) {
            style |= WS_MAXIMIZE;
        }
        if self.contains(WindowFlags::IGNORE_CURSOR_EVENT) {
            style_ex |= WS_EX_TRANSPARENT | WS_EX_LAYERED;
        }
        if self.contains(WindowFlags::CLIP_CHILDREN) {
            style |= WS_CLIPCHILDREN;
        }

        if self.intersects(
            WindowFlags::MARKER_EXCLUSIVE_FULLSCREEN | WindowFlags::MARKER_BORDERLESS_FULLSCREEN,
        ) {
            style &= !WS_OVERLAPPEDWINDOW;
        }

        (style, style_ex)
    }

    /// aterm: the `ShowWindow` command [`Self::apply_diff`] shows a window with, going
    /// from `old` to `new` (visible). No activation for a window that asked for none; for a
    /// hidden window revealed maximized, `SW_SHOWMAXIMIZED` (`SW_MAXIMIZE`, the same value),
    /// which shows it maximized in one step instead of at its normal frame first and then
    /// maximized on glass.
    fn show_command(old: WindowFlags, new: WindowFlags) -> SHOW_WINDOW_CMD {
        if !old.contains(WindowFlags::MARKER_ACTIVATE) {
            SW_SHOWNOACTIVATE
        } else if !old.contains(WindowFlags::VISIBLE) && new.contains(WindowFlags::MAXIMIZED) {
            SW_MAXIMIZE
        } else {
            SW_SHOW
        }
    }

    /// aterm: whether a maximize waits for the window's reveal ([`Self::show_command`]):
    /// a hidden window's. `SW_MAXIMIZE` would show it, and `apply_diff`'s `SW_HIDE` hide it
    /// again, on glass; nor does it take the `WS_MAXIMIZE` style early, which `ShowWindow`
    /// reads as "already maximized" and would then show it at its normal frame.
    fn maximize_waits_for_reveal(new: WindowFlags) -> bool {
        new.contains(WindowFlags::MAXIMIZED) && !new.contains(WindowFlags::VISIBLE)
    }

    /// Adjust the window client rectangle to the return value, if present.
    fn apply_diff(mut self, window: HWND, mut new: WindowFlags) {
        self = self.mask();
        new = new.mask();

        let mut diff = self ^ new;

        if diff == WindowFlags::empty() {
            return;
        }

        if new.contains(WindowFlags::VISIBLE) {
            unsafe {
                ShowWindow(window, Self::show_command(self, new));
            }
        }

        if diff.intersects(WindowFlags::ALWAYS_ON_TOP | WindowFlags::ALWAYS_ON_BOTTOM) {
            unsafe {
                SetWindowPos(
                    window,
                    match (
                        new.contains(WindowFlags::ALWAYS_ON_TOP),
                        new.contains(WindowFlags::ALWAYS_ON_BOTTOM),
                    ) {
                        (true, false) => HWND_TOPMOST,
                        (false, false) => HWND_NOTOPMOST,
                        (false, true) => HWND_BOTTOM,
                        (true, true) => unreachable!(),
                    },
                    0,
                    0,
                    0,
                    0,
                    SWP_ASYNCWINDOWPOS | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                InvalidateRgn(window, std::ptr::null_mut(), false.into());
            }
        }

        let maximize_hidden = Self::maximize_waits_for_reveal(new);
        if (diff.contains(WindowFlags::MAXIMIZED) || new.contains(WindowFlags::MAXIMIZED))
            && !maximize_hidden
        {
            unsafe {
                ShowWindow(window, match new.contains(WindowFlags::MAXIMIZED) {
                    true => SW_MAXIMIZE,
                    false => SW_RESTORE,
                });
            }
        }

        if diff.contains(WindowFlags::MINIMIZED) {
            unsafe {
                ShowWindow(window, match new.contains(WindowFlags::MINIMIZED) {
                    true => SW_MINIMIZE,
                    false => SW_RESTORE,
                });
            }

            diff.remove(WindowFlags::MINIMIZED);
        }

        if diff.contains(WindowFlags::CLOSABLE) || new.contains(WindowFlags::CLOSABLE) {
            let flags = MF_BYCOMMAND
                | if new.contains(WindowFlags::CLOSABLE) { MF_ENABLED } else { MF_DISABLED };

            unsafe {
                EnableMenuItem(GetSystemMenu(window, 0), SC_CLOSE, flags);
            }
        }

        if !new.contains(WindowFlags::VISIBLE) {
            unsafe {
                ShowWindow(window, SW_HIDE);
            }
        }

        if diff != WindowFlags::empty() {
            let styled = if maximize_hidden { new - WindowFlags::MAXIMIZED } else { new };
            let (style, style_ex) = styled.to_window_styles();

            unsafe {
                SendMessageW(window, event_loop::SET_RETAIN_STATE_ON_SIZE_MSG_ID.get(), 1, 0);

                if !new.contains(WindowFlags::MINIMIZED) {
                    SetWindowLongW(window, GWL_STYLE, style as i32);
                    SetWindowLongW(window, GWL_EXSTYLE, style_ex as i32);
                }

                let mut flags = SWP_NOZORDER | SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED;

                if !new.contains(WindowFlags::MARKER_EXCLUSIVE_FULLSCREEN)
                    && !new.contains(WindowFlags::MARKER_BORDERLESS_FULLSCREEN)
                {
                    flags |= SWP_NOACTIVATE;
                }

                SetWindowPos(window, std::ptr::null_mut(), 0, 0, 0, 0, flags);
                SendMessageW(window, event_loop::SET_RETAIN_STATE_ON_SIZE_MSG_ID.get(), 0, 0);
            }
        }
    }

    pub fn adjust_rect(self, hwnd: HWND, mut rect: RECT) -> Result<RECT, io::Error> {
        unsafe {
            let mut style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let style_ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;

            if !self.contains(WindowFlags::MARKER_DECORATIONS) {
                style &= !(WS_CAPTION | WS_SIZEBOX);
            }

            util::win_to_err({
                let b_menu = !GetMenu(hwnd).is_null();
                if let (Some(get_dpi_for_window), Some(adjust_window_rect_ex_for_dpi)) =
                    (*util::GET_DPI_FOR_WINDOW, *util::ADJUST_WINDOW_RECT_EX_FOR_DPI)
                {
                    let dpi = get_dpi_for_window(hwnd);
                    adjust_window_rect_ex_for_dpi(&mut rect, style, b_menu.into(), style_ex, dpi)
                } else {
                    AdjustWindowRectEx(&mut rect, style, b_menu.into(), style_ex)
                }
            })?;
            Ok(rect)
        }
    }

    pub fn adjust_size(self, hwnd: HWND, size: PhysicalSize<u32>) -> PhysicalSize<u32> {
        let (width, height): (u32, u32) = size.into();
        let rect = RECT { left: 0, right: width as i32, top: 0, bottom: height as i32 };
        let rect = self.adjust_rect(hwnd, rect).unwrap_or(rect);

        let outer_x = (rect.right - rect.left).abs();
        let outer_y = (rect.top - rect.bottom).abs();

        PhysicalSize::new(outer_x as _, outer_y as _)
    }

    pub fn set_size(self, hwnd: HWND, size: PhysicalSize<u32>) {
        unsafe {
            let (width, height): (u32, u32) = self.adjust_size(hwnd, size).into();
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                width as _,
                height as _,
                SWP_ASYNCWINDOWPOS | SWP_NOZORDER | SWP_NOREPOSITION | SWP_NOMOVE | SWP_NOACTIVATE,
            );
            InvalidateRgn(hwnd, std::ptr::null_mut(), false.into());
        }
    }
}

impl CursorFlags {
    fn refresh_os_cursor(self, window: HWND) -> Result<(), io::Error> {
        let client_rect = util::WindowArea::Inner.get_rect(window)?;

        if util::is_focused(window) {
            let cursor_clip = match self.contains(CursorFlags::GRABBED) {
                true => {
                    if self.contains(CursorFlags::LOCKED) {
                        if let Ok(pos) = util::get_cursor_position() {
                            Some(RECT {
                                left: pos.x,
                                right: pos.x + 1,
                                top: pos.y,
                                bottom: pos.y + 1,
                            })
                        } else {
                            let cx = (client_rect.left + client_rect.right) / 2;
                            let cy = (client_rect.top + client_rect.bottom) / 2;
                            Some(RECT { left: cx, right: cx + 1, top: cy, bottom: cy + 1 })
                        }
                    } else if self.contains(CursorFlags::HIDDEN) {
                        let cx = (client_rect.left + client_rect.right) / 2;
                        let cy = (client_rect.top + client_rect.bottom) / 2;
                        Some(RECT { left: cx, right: cx + 1, top: cy, bottom: cy + 1 })
                    } else {
                        Some(client_rect)
                    }
                },
                false => None,
            };

            let rect_to_tuple = |rect: RECT| (rect.left, rect.top, rect.right, rect.bottom);
            let active_cursor_clip = rect_to_tuple(util::get_cursor_clip()?);
            let desktop_rect = rect_to_tuple(util::get_desktop_rect());

            let active_cursor_clip = match desktop_rect == active_cursor_clip {
                true => None,
                false => Some(active_cursor_clip),
            };

            if active_cursor_clip != cursor_clip.map(rect_to_tuple) {
                util::set_cursor_clip(cursor_clip)?;
            }
        }

        let cursor_in_client = self.contains(CursorFlags::IN_WINDOW);
        if cursor_in_client {
            util::set_cursor_hidden(self.contains(CursorFlags::HIDDEN));
        } else {
            util::set_cursor_hidden(false);
        }

        Ok(())
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::WindowFlags;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_MAXIMIZE, SW_SHOW, SW_SHOWNOACTIVATE};

    /// aterm: a hidden window maximized before its reveal keeps the maximize for the
    /// reveal, which shows it maximized in one step; a shown window maximizes at once.
    #[test]
    fn a_hidden_window_is_revealed_maximized_in_one_step() {
        let hidden = WindowFlags::MARKER_ACTIVATE | WindowFlags::RESIZABLE;
        let hidden_max = hidden | WindowFlags::MAXIMIZED;
        let shown = hidden | WindowFlags::VISIBLE;
        assert!(WindowFlags::maximize_waits_for_reveal(hidden_max));
        assert!(!WindowFlags::maximize_waits_for_reveal(shown | WindowFlags::MAXIMIZED));
        assert!(!WindowFlags::maximize_waits_for_reveal(hidden));
        assert_eq!(
            WindowFlags::show_command(hidden_max, hidden_max | WindowFlags::VISIBLE),
            SW_MAXIMIZE
        );
        assert_eq!(WindowFlags::show_command(hidden, shown), SW_SHOW);
        assert_eq!(WindowFlags::show_command(shown, shown | WindowFlags::MAXIMIZED), SW_SHOW);
        let passive = WindowFlags::MAXIMIZED;
        assert_eq!(
            WindowFlags::show_command(passive, passive | WindowFlags::VISIBLE),
            SW_SHOWNOACTIVATE
        );
    }
}
