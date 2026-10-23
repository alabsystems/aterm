// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/window.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::ffi::CString;
use std::mem::replace;
use std::os::raw::*;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use std::{cmp, env};

use aterm_types::native_window::NativeWindow;
use aterm_x11::icccm::{Specified, WmHints, WmSizeHints, WM_HINTS_ITEMS, WM_SIZE_HINTS_ITEMS};
use aterm_x11::proto::xproto::{self, cw, PropMode};
use aterm_x11::proto::{atom, xinput, ValueList, COPY_FROM_PARENT, CURRENT_TIME};
use tracing::{debug, info, warn};

use crate::cursor::Cursor;
use crate::cursor_icon::CursorIcon;
use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{ExternalError, NotSupportedError, OsError as RootOsError};
use crate::event::{Event, InnerSizeWriter, WindowEvent};
use crate::platform_impl::x11::atoms::*;
use crate::platform_impl::x11::{
    xinput_fp1616_to_float, MonitorHandle as X11MonitorHandle, WakeSender, X11Error,
};
use crate::platform_impl::{
    Fullscreen, MonitorHandle as PlatformMonitorHandle, OsError, PlatformIcon,
};
use crate::window::{ImePurpose, ResizeDirection, Theme, UserAttentionType, WindowAttributes};

use super::sync_request::SyncCounter;
use super::util;
use super::{
    ffi, ActiveEventLoop, CookieResultExt, IgnoreError as _, ImeRequest, ImeSender, ReportExt,
    VoidCookie, WindowId, XConnection,
};

// LOCAL PATCH (aterm): the requests are aterm-x11's encoders and ICCCM properties, where upstream
// used x11rb's; there is no input shape (`set_cursor_hittest` is Windows-only). A failed request is
// reported (`XConnection::report`), never a panic, so the event loop can end the run on a lost
// display. Where the server has SYNC the window offers EWMH's resize sync counter
// (`sync_request.rs`).

#[derive(Debug)]
pub struct SharedState {
    pub cursor_pos: Option<(f64, f64)>,
    pub size: Option<(u32, u32)>,
    pub position: Option<(i32, i32)>,
    pub inner_position: Option<(i32, i32)>,
    pub inner_position_rel_parent: Option<(i32, i32)>,
    pub last_monitor: X11MonitorHandle,
    pub dpi_adjusted: Option<(u32, u32)>,
    pub(crate) fullscreen: Option<Fullscreen>,
    pub(crate) desired_fullscreen: Option<Option<Fullscreen>>,
    pub restore_position: Option<(i32, i32)>,
    pub frame_extents: Option<util::FrameExtentsHeuristic>,
    pub min_inner_size: Option<Size>,
    pub resize_increments: Option<Size>,
    pub visibility: Visibility,
    pub has_focus: bool,
    /// Whether this client set the window's urgency hint (`request_user_attention`), which it clears when the window gains focus.
    pub urgent: bool,
    /// What hides the window, and what `WindowEvent::Occluded` last said of it.
    pub occlusion: Occlusion,
    /// Whether the server asked for the window to be drawn again since its last `RedrawRequested` (`Expose`).
    pub exposed: bool,
    /// What `_NET_WM_STATE` held at its last read, until the window manager changes it
    /// (`UnownedWindow::wm_state_changed`).
    shown: Option<ShownState>,
    /// How many changes of `_NET_WM_STATE` the window has seen: a read that one overtook is not kept.
    shown_changes: u64,
}

/// The `_NET_WM_STATE` atoms that say how the window manager shows the window: hidden, or in a
/// frame it gives the window (`UnownedWindow::shown_state`).
#[derive(Clone, Copy, Debug, Default)]
struct ShownState {
    hidden: bool,
    maximized_horz: bool,
    maximized_vert: bool,
    fullscreen: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Visibility {
    No,
    Yes,
    YesWait,
}

/// LOCAL PATCH (aterm), 2026-10-22.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Occlusion {
    /// Unmapped since it was last mapped.
    pub unmapped: bool,
    /// Its `_NET_WM_STATE` holds `_NET_WM_STATE_HIDDEN`.
    pub hidden: bool,
    /// The last VisibilityNotify said `VisibilityFullyObscured`.
    pub obscured: bool,
    /// What the last `WindowEvent::Occluded` said; `false` before the first.
    reported: bool,
}

impl Occlusion {
    /// Applies `change`, and returns whether the window is occluded now when that is not what it last reported.
    pub fn change(&mut self, change: impl FnOnce(&mut Self)) -> Option<bool> {
        change(self);
        let occluded = self.unmapped || self.hidden || self.obscured;
        (replace(&mut self.reported, occluded) != occluded).then_some(occluded)
    }
}

impl SharedState {
    fn new(last_monitor: X11MonitorHandle, window_attributes: &WindowAttributes) -> Mutex<Self> {
        let visibility =
            if window_attributes.visible { Visibility::YesWait } else { Visibility::No };

        Mutex::new(SharedState {
            last_monitor,
            visibility,

            cursor_pos: None,
            size: None,
            position: None,
            inner_position: None,
            inner_position_rel_parent: None,
            dpi_adjusted: None,
            fullscreen: None,
            desired_fullscreen: None,
            restore_position: None,
            frame_extents: None,
            min_inner_size: None,
            resize_increments: None,
            has_focus: false,
            urgent: false,
            occlusion: Occlusion::default(),
            exposed: false,
            shown: None,
            shown_changes: 0,
        })
    }
}

unsafe impl Send for UnownedWindow {}
unsafe impl Sync for UnownedWindow {}

pub struct UnownedWindow {
    pub(crate) xconn: Arc<XConnection>,
    xwindow: xproto::Window,
    visual: u32,
    root: xproto::Window,
    screen_id: i32,
    /// The inner size the window was created with, what [`Self::inner_size`] answers when the
    /// server cannot be asked and has never reported one ([`Self::inner_size_or_last`]).
    created_size: (u32, u32),
    selected_cursor: Mutex<CursorIcon>,
    ime_sender: Mutex<ImeSender>,
    pub shared_state: Mutex<SharedState>,
    redraw_sender: WakeSender<WindowId>,
    /// The launch's startup id until the window first maps, when its sequence ends
    /// (`end_startup_sequence`).
    startup_id: Mutex<Option<String>>,
    /// The resize sync counter the window offers its window manager, where the server has SYNC.
    sync: Option<SyncCounter>,
}

macro_rules! leap {
    ($e:expr) => {
        match $e {
            Ok(x) => x,
            Err(err) => return Err(os_error!(OsError::XError(X11Error::from(err).into()))),
        }
    };
}

/// `NorthWestGravity` (X11 protocol): a resize keeps the window's contents at its top-left.
const NORTH_WEST_GRAVITY: u32 = 1;

/// The `_GTK_THEME_VARIANT` a window's `theme` asks for.
pub(super) fn theme_variant(theme: Option<Theme>) -> Option<&'static str> {
    match theme {
        Some(Theme::Dark) => Some("dark"),
        Some(Theme::Light) => Some("light"),
        None => None,
    }
}

/// Encode `rgb` (`0x00RRGGBB`) as a pixel of a `TrueColor` or `DirectColor` `visual` on `screen`.
pub(super) fn background_pixel(
    screen: &xproto::Screen,
    visual: xproto::Visualid,
    rgb: u32,
) -> Option<u32> {
    /// `TrueColor` and `DirectColor` (X11 protocol).
    const DECOMPOSED: [u8; 2] = [4, 5];
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| &depth.visuals)
        .find(|v| v.visual_id == visual)?;
    if !DECOMPOSED.contains(&visual.class) {
        return None;
    }
    let channel = |value: u32, mask: u32| {
        if mask == 0 {
            return 0;
        }
        let shift = mask.trailing_zeros();
        let max = u64::from(mask >> shift);
        let scaled = (u64::from(value & 0xff) * max + 127) / 255;
        (u32::try_from(scaled).unwrap_or(u32::MAX) << shift) & mask
    };
    Some(
        channel(rgb >> 16, visual.red_mask)
            | channel(rgb >> 8, visual.green_mask)
            | channel(rgb, visual.blue_mask),
    )
}

impl UnownedWindow {
    #[allow(clippy::unnecessary_cast)]
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        window_attrs: WindowAttributes,
    ) -> Result<UnownedWindow, RootOsError> {
        let xconn = &event_loop.xconn;
        let atoms = xconn.atoms();

        let screen_id = xconn.default_screen_index() as c_int;
        let root = event_loop.root;

        let mut monitors = leap!(xconn.available_monitors());
        let guessed_monitor = if monitors.is_empty() {
            X11MonitorHandle::dummy()
        } else {
            xconn
                .query_pointer(root, util::VIRTUAL_CORE_POINTER)
                .ok()
                .and_then(|pointer_state| {
                    let (x, y) = (pointer_state.root_x as i64, pointer_state.root_y as i64);

                    for i in 0..monitors.len() {
                        if monitors[i].rect.contains_point(x, y) {
                            return Some(monitors.swap_remove(i));
                        }
                    }

                    None
                })
                .unwrap_or_else(|| monitors.swap_remove(0))
        };
        let scale_factor = guessed_monitor.scale_factor();

        info!("Guessed window scale factor: {}", scale_factor);

        let min_inner_size: Option<(u32, u32)> =
            window_attrs.min_inner_size.map(|size| size.to_physical::<u32>(scale_factor).into());

        let dimensions = {
            let mut dimensions: (u32, u32) = window_attrs
                .inner_size
                .map(|size| size.to_physical::<u32>(scale_factor))
                .or_else(|| Some((800, 600).into()))
                .map(Into::into)
                .unwrap();
            if let Some(min) = min_inner_size {
                dimensions.0 = cmp::max(dimensions.0, min.0);
                dimensions.1 = cmp::max(dimensions.1, min.1);
            }
            debug!("Calculated physical dimensions: {}x{}", dimensions.0, dimensions.1);
            dimensions
        };

        let depth = COPY_FROM_PARENT as u8;
        let mut visual = COPY_FROM_PARENT;

        let window_attributes = {
            use xproto::event_mask;

            let event_mask = event_mask::EXPOSURE
                | event_mask::STRUCTURE_NOTIFY
                | event_mask::VISIBILITY_CHANGE
                | event_mask::KEY_PRESS
                | event_mask::KEY_RELEASE
                | event_mask::KEYMAP_STATE
                | event_mask::BUTTON_PRESS
                | event_mask::BUTTON_RELEASE
                | event_mask::POINTER_MOTION
                | event_mask::PROPERTY_CHANGE;

            let values = ValueList::new()
                .set(cw::EVENT_MASK, event_mask)
                .set(cw::BORDER_PIXEL, 0)
                .set(cw::COLORMAP, 0);
            let screen = xconn.default_root();
            match window_attrs
                .platform_specific
                .x11_background
                .and_then(|rgb| background_pixel(screen, screen.root_visual, rgb))
            {
                Some(pixel) => {
                    values.set(cw::BACK_PIXEL, pixel).set(cw::BIT_GRAVITY, NORTH_WEST_GRAVITY)
                },
                None => values,
            }
        };

        let xwindow = {
            let wid = leap!(xconn.xcb_connection().generate_id());
            let result = xconn.send_void(xproto::create_window(
                depth,
                wid,
                root,
                0,
                0,
                dimensions.0.try_into().unwrap(),
                dimensions.1.try_into().unwrap(),
                0,
                xproto::window_class::INPUT_OUTPUT,
                visual,
                &window_attributes,
            ));
            leap!(leap!(result).check());

            wid
        };

        if visual == COPY_FROM_PARENT {
            visual = leap!(xconn.request(xproto::get_window_attributes(xwindow))).visual;
        }

        #[allow(clippy::mutex_atomic)]
        let mut window = UnownedWindow {
            xconn: Arc::clone(xconn),
            xwindow: xwindow as xproto::Window,
            visual,
            root,
            screen_id,
            created_size: dimensions,
            selected_cursor: Default::default(),
            ime_sender: Mutex::new(event_loop.ime_sender.clone()),
            shared_state: SharedState::new(guessed_monitor, &window_attrs),
            redraw_sender: event_loop.redraw_sender.clone(),
            startup_id: Mutex::new(window_attrs.platform_specific.activation_token.clone()),
            sync: None,
        };

        leap!(window.set_title_inner(&window_attrs.title)).ignore_error();
        leap!(window.set_decorations_inner()).ignore_error();

        {
            {
                let dnd_aware_atom = atoms[XdndAware];
                let version = &[5u32];
                leap!(xconn.change_property(
                    window.xwindow,
                    dnd_aware_atom,
                    atom::ATOM,
                    xproto::PropMode::Replace,
                    version,
                ))
                .ignore_error();
            }

            {
                let (instance, class) = if let Some(name) = window_attrs.platform_specific.name {
                    (name.instance, name.general)
                } else {
                    let class = env::args_os()
                        .next()
                        .as_ref()
                        .and_then(|path| Path::new(path).file_name())
                        .and_then(|bin_name| bin_name.to_str())
                        .map(|bin_name| bin_name.to_owned())
                        .unwrap_or_else(|| window_attrs.title.clone());
                    let instance = env::var("RESOURCE_NAME").ok().unwrap_or_else(|| class.clone());
                    (instance, class)
                };

                let class = format!("{instance}\0{class}\0");
                leap!(xconn.change_property(
                    window.xwindow,
                    atom::WM_CLASS,
                    atom::STRING,
                    xproto::PropMode::Replace,
                    class.as_bytes(),
                ))
                .ignore_error();
            }

            if let Some(flusher) = leap!(window.set_pid()) {
                flusher.ignore_error()
            }

            if let Some(startup_id) = window_attrs.platform_specific.activation_token.as_deref() {
                leap!(xconn.set_startup_id(window.xwindow, startup_id)).ignore_error();
            }

            leap!(xconn.change_property(
                window.xwindow,
                atoms[_NET_WM_WINDOW_TYPE],
                atom::ATOM,
                xproto::PropMode::Replace,
                &[atoms[_NET_WM_WINDOW_TYPE_NORMAL]],
            ))
            .ignore_error();

            let min_inner_size =
                window_attrs.min_inner_size.map(|size| size.to_physical::<u32>(scale_factor));

            let shared_state = window.shared_state.get_mut().unwrap();
            shared_state.min_inner_size = min_inner_size.map(Into::into);

            let normal_hints = WmSizeHints {
                position: None,
                size: Some((
                    Specified::User,
                    cast_dimension_to_hint(dimensions.0),
                    cast_dimension_to_hint(dimensions.1),
                )),
                max_size: None,
                min_size: min_inner_size.map(cast_physical_size_to_hint),
                size_increment: None,
                base_size: None,
                aspect: None,
                win_gravity: None,
            };
            leap!(leap!(xconn.send_void(normal_hints.set(window.xwindow, atom::WM_NORMAL_HINTS)))
                .check());

            if let Some(icon) = window_attrs.window_icon {
                leap!(window.set_icon_inner(icon.inner)).ignore_error();
            }

            window.sync = SyncCounter::offer(xconn, window.xwindow);
            let protocols =
                [atoms[WM_DELETE_WINDOW], atoms[_NET_WM_PING], atoms[_NET_WM_SYNC_REQUEST]];
            let offered = if window.sync.is_some() { 3 } else { 2 };
            let result = xconn.send_void(xproto::change_property32(
                PropMode::Replace,
                window.xwindow,
                atoms[WM_PROTOCOLS],
                atom::ATOM,
                &protocols[..offered],
            ));
            leap!(result).ignore_error();

            let mask = xinput::event_mask::MOTION
                | xinput::event_mask::BUTTON_PRESS
                | xinput::event_mask::BUTTON_RELEASE
                | xinput::event_mask::ENTER
                | xinput::event_mask::LEAVE
                | xinput::event_mask::FOCUS_IN
                | xinput::event_mask::FOCUS_OUT
                | xinput::event_mask::TOUCH_BEGIN
                | xinput::event_mask::TOUCH_UPDATE
                | xinput::event_mask::TOUCH_END;
            leap!(xconn.select_xinput_events(window.xwindow, super::ALL_MASTER_DEVICES, mask))
                .ignore_error();

            if window_attrs.visible {
                leap!(xconn.send_void(xproto::map_window(window.xwindow))).ignore_error();
                leap!(xconn.send_void(xproto::configure_window(xwindow, &stack_above())))
                    .ignore_error();
                window.end_startup_sequence();
            }

            unsafe {
                let mut supported_ptr = ffi::FALSE;
                (xconn.xlib.XkbSetDetectableAutoRepeat)(
                    xconn.display,
                    ffi::TRUE,
                    &mut supported_ptr,
                );
                if supported_ptr == ffi::FALSE {
                    return Err(os_error!(OsError::Misc("`XkbSetDetectableAutoRepeat` failed")));
                }
            }

            if let Some(ime) = event_loop.ime.as_ref() {
                let result = ime.borrow_mut().create_context(window.xwindow as ffi::Window, false);
                leap!(result);
            }

            leap!(window.clear_window_level()).ignore_error();
        }

        let window = leap!(xconn.sync_with_server().map(|_| window));

        Ok(window)
    }

    pub(super) fn shared_state_lock(&self) -> MutexGuard<'_, SharedState> {
        self.shared_state.lock().unwrap()
    }

    /// The window manager sent a `_NET_WM_SYNC_REQUEST` for `value` (`sync_request.rs`, step 1).
    pub(super) fn sync_requested(&self, value: i64) {
        if let Some(sync) = &self.sync {
            sync.requested(value);
        }
    }

    /// A `ConfigureNotify` came, which changed the window's size or (`resized` false) did not (`sync_request.rs`, step 2).
    pub(super) fn sync_configured(&self, resized: bool) {
        if let Some(sync) = &self.sync {
            sync.configured(&self.xconn, resized);
        }
    }

    /// The application presented a frame of the window (`WindowExtX11::x11_frame_presented`, `sync_request.rs`, step 3).
    pub fn frame_presented(&self) {
        if let Some(sync) = &self.sync {
            sync.presented(&self.xconn);
        }
    }

    /// An event-loop turn ended at `now`, or the loop woke for [`Self::sync_deadline`].
    pub(super) fn sync_turn_ended(&self, now: Instant, redraw_queued: impl FnOnce() -> bool) {
        if let Some(sync) = &self.sync {
            sync.turn_ended(&self.xconn, now, redraw_queued);
        }
    }

    /// When the event loop must wake to answer the window manager's pending sync request (`sync_request.rs`, step 5).
    pub(super) fn sync_deadline(&self) -> Option<Instant> {
        self.sync.as_ref().and_then(SyncCounter::deadline)
    }

    fn set_pid(&self) -> Result<Option<VoidCookie<'_>>, X11Error> {
        let atoms = self.xconn.atoms();
        let pid_atom = atoms[_NET_WM_PID];
        let client_machine_atom = atoms[WM_CLIENT_MACHINE];

        let hostname = super::xdisplay::hostname();
        // SAFETY: getpid(2) cannot fail.
        let pid = unsafe { libc::getpid() };

        self.xconn
            .change_property(self.xwindow, pid_atom, atom::CARDINAL, xproto::PropMode::Replace, &[
                pid as util::Cardinal,
            ])?
            .ignore_error();
        let flusher = self.xconn.change_property(
            self.xwindow,
            client_machine_atom,
            atom::STRING,
            xproto::PropMode::Replace,
            std::os::unix::ffi::OsStrExt::as_bytes(hostname.as_os_str()),
        );
        flusher.map(Some)
    }

    /// Set `_GTK_THEME_VARIANT` to `theme` ([`theme_variant`]), or remove it when `None`.
    pub fn set_theme_inner(&self, theme: Option<Theme>) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();
        let hint_atom = atoms[_GTK_THEME_VARIANT];
        let utf8_atom = atoms[UTF8_STRING];
        let Some(variant) = theme_variant(theme) else {
            return self.xconn.send_void(xproto::delete_property(self.xwindow, hint_atom));
        };
        let variant = CString::new(variant).expect("`_GTK_THEME_VARIANT` contained null byte");
        self.xconn.change_property(
            self.xwindow,
            hint_atom,
            utf8_atom,
            xproto::PropMode::Replace,
            variant.as_bytes(),
        )
    }

    /// The background the X server paints the window's exposed areas (see `platform::x11::WindowExtX11::set_x11_background`).
    pub fn set_background(&self, rgb: u32) {
        let Some(pixel) = background_pixel(self.xconn.default_root(), self.visual, rgb) else {
            return;
        };
        let what = "Failed to change window background";
        self.xconn
            .send_void(xproto::change_window_attributes(
                self.xwindow,
                &ValueList::new().set(cw::BACK_PIXEL, pixel),
            ))
            .ignore_error_or_report(&self.xconn, what);
        self.xconn.flush_requests().or_report(&self.xconn, what);
    }

    #[inline]
    pub fn set_theme(&self, theme: Option<Theme>) {
        let what = "Failed to change window theme";
        self.set_theme_inner(theme).ignore_error_or_report(&self.xconn, what);

        self.xconn.flush_requests().or_report(&self.xconn, what);
    }

    fn set_netwm(
        &self,
        operation: util::StateOperation,
        properties: (u32, u32, u32, u32),
    ) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();
        let state_atom = atoms[_NET_WM_STATE];
        self.xconn.send_client_msg(
            self.xwindow,
            self.root,
            state_atom,
            Some(
                xproto::event_mask::SUBSTRUCTURE_REDIRECT | xproto::event_mask::SUBSTRUCTURE_NOTIFY,
            ),
            [operation as u32, properties.0, properties.1, properties.2, properties.3],
        )
    }

    /// Removes `_NET_WM_STATE_ABOVE` and then `_NET_WM_STATE_BELOW`, the two messages winit's
    /// creation-time `set_window_level(WindowLevel::Normal)` sent for every window.
    fn clear_window_level(&self) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();
        self.set_netwm(util::StateOperation::Remove, (atoms[_NET_WM_STATE_ABOVE], 0, 0, 0))?
            .ignore_error();
        self.set_netwm(util::StateOperation::Remove, (atoms[_NET_WM_STATE_BELOW], 0, 0, 0))
    }

    fn set_fullscreen_hint(&self, fullscreen: bool) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();
        let fullscreen_atom = atoms[_NET_WM_STATE_FULLSCREEN];
        let flusher = self.set_netwm(fullscreen.into(), (fullscreen_atom, 0, 0, 0));

        if fullscreen {
            self.xconn
                .send_void(xproto::set_input_focus(
                    xproto::input_focus::PARENT,
                    self.xwindow,
                    CURRENT_TIME,
                ))?
                .ignore_error();
        }

        flusher
    }

    fn set_fullscreen_inner(
        &self,
        fullscreen: Option<Fullscreen>,
    ) -> Result<Option<VoidCookie<'_>>, X11Error> {
        let mut shared_state_lock = self.shared_state_lock();

        match shared_state_lock.visibility {
            Visibility::No | Visibility::YesWait => {
                shared_state_lock.desired_fullscreen = Some(fullscreen);
                return Ok(None);
            },
            Visibility::Yes => (),
        }

        if shared_state_lock.fullscreen == fullscreen {
            return Ok(None);
        }
        let shown = std::mem::replace(&mut shared_state_lock.fullscreen, fullscreen.clone());

        drop(shared_state_lock);

        match fullscreen {
            None => {
                let flusher = self.set_fullscreen_hint(false);
                let mut shared_state_lock = self.shared_state_lock();
                if let Some(position) = shared_state_lock.restore_position.take() {
                    drop(shared_state_lock);
                    self.set_position_inner(position.0, position.1)
                        .ignore_error_or_report(&self.xconn, "Failed to restore window position");
                }
                flusher.map(Some)
            },
            Some(fullscreen) => {
                let monitor = match fullscreen {
                    Fullscreen::Borderless(Some(PlatformMonitorHandle::X(monitor))) => monitor,
                    Fullscreen::Borderless(None) => self.shared_state_lock().last_monitor.clone(),
                    #[cfg(wayland_platform)]
                    _ => unreachable!(),
                };

                if monitor.is_dummy() {
                    return Ok(None);
                }

                let window_position = match self.outer_position_physical() {
                    Ok(position) => position,
                    Err(error) => {
                        self.shared_state_lock().fullscreen = shown;
                        return Err(error);
                    },
                };
                self.shared_state_lock().restore_position = Some(window_position);
                let monitor_origin: (i32, i32) = monitor.position().into();
                self.set_position_inner(monitor_origin.0, monitor_origin.1)
                    .ignore_error_or_report(&self.xconn, "Failed to set window position");
                self.set_fullscreen_hint(true).map(Some)
            },
        }
    }

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        let shared_state = self.shared_state_lock();

        shared_state.desired_fullscreen.clone().unwrap_or_else(|| shared_state.fullscreen.clone())
    }

    #[inline]
    pub(crate) fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let what = "Failed to change window fullscreen state";
        if let Some(Some(flusher)) =
            self.set_fullscreen_inner(fullscreen).or_report(&self.xconn, what)
        {
            flusher.check().or_report(&self.xconn, what);
            self.invalidate_cached_frame_extents();
        }
    }

    pub(crate) fn visibility_notify(&self) {
        let mut shared_state = self.shared_state_lock();

        match shared_state.visibility {
            Visibility::No => self
                .xconn
                .send_void(xproto::unmap_window(self.xwindow))
                .ignore_error_or_report(&self.xconn, "Failed to unmap window"),
            Visibility::Yes => (),
            Visibility::YesWait => {
                shared_state.visibility = Visibility::Yes;

                if let Some(fullscreen) = shared_state.desired_fullscreen.take() {
                    drop(shared_state);
                    self.set_fullscreen(fullscreen);
                }
            },
        }
    }

    pub fn current_monitor(&self) -> Option<X11MonitorHandle> {
        Some(self.shared_state_lock().last_monitor.clone())
    }

    pub fn available_monitors(&self) -> Vec<X11MonitorHandle> {
        self.xconn
            .available_monitors()
            .or_report(&self.xconn, "Failed to get available monitors")
            .unwrap_or_default()
    }

    pub fn primary_monitor(&self) -> Option<X11MonitorHandle> {
        self.xconn.primary_monitor().or_report(&self.xconn, "Failed to get primary monitor")
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        Some(self.shown_state().hidden)
    }

    /// Refresh the API for the given monitor.
    #[inline]
    pub(super) fn refresh_dpi_for_monitor<T: 'static>(
        &self,
        new_monitor: &X11MonitorHandle,
        maybe_prev_scale_factor: Option<f64>,
        mut callback: impl FnMut(Event<T>),
    ) {
        let monitor = self.shared_state_lock().last_monitor.clone();
        if monitor.name == new_monitor.name {
            let (width, height) = self.inner_size_or_last();
            let (new_width, new_height) = self.adjust_for_dpi(
                maybe_prev_scale_factor.unwrap_or(monitor.scale_factor),
                new_monitor.scale_factor,
                width,
                height,
                &self.shared_state_lock(),
            );

            let window_id = crate::window::WindowId(self.id());
            let old_inner_size = PhysicalSize::new(width, height);
            let inner_size = Arc::new(Mutex::new(PhysicalSize::new(new_width, new_height)));
            callback(Event::WindowEvent {
                window_id,
                event: WindowEvent::ScaleFactorChanged {
                    scale_factor: new_monitor.scale_factor,
                    inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&inner_size)),
                },
            });

            let new_inner_size = *inner_size.lock().unwrap();
            drop(inner_size);

            if new_inner_size != old_inner_size {
                let (new_width, new_height) = new_inner_size.into();
                self.request_inner_size_physical(new_width, new_height);
            }
        }
    }

    fn set_minimized_inner(&self, minimized: bool) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();

        if minimized {
            let root_window = self.xconn.default_root().root;

            self.xconn.send_client_msg(
                self.xwindow,
                root_window,
                atoms[WM_CHANGE_STATE],
                Some(
                    xproto::event_mask::SUBSTRUCTURE_REDIRECT
                        | xproto::event_mask::SUBSTRUCTURE_NOTIFY,
                ),
                [3u32, 0, 0, 0, 0],
            )
        } else {
            self.xconn.send_client_msg(
                self.xwindow,
                self.root,
                atoms[_NET_ACTIVE_WINDOW],
                Some(
                    xproto::event_mask::SUBSTRUCTURE_REDIRECT
                        | xproto::event_mask::SUBSTRUCTURE_NOTIFY,
                ),
                [1, CURRENT_TIME, 0, 0, 0],
            )
        }
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        let what = "Failed to change window minimization";
        self.set_minimized_inner(minimized).ignore_error_or_report(&self.xconn, what);

        self.xconn.flush_requests().or_report(&self.xconn, what);
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        let shown = self.shown_state();
        shown.maximized_horz && shown.maximized_vert
    }

    /// Maximized in exactly one direction.
    #[inline]
    pub fn is_tiled(&self) -> bool {
        let shown = self.shown_state();
        shown.maximized_horz != shown.maximized_vert
    }

    /// Fullscreen state from `_NET_WM_STATE_FULLSCREEN` or a pending [`Self::fullscreen`] request.
    #[inline]
    pub fn is_shown_fullscreen(&self) -> bool {
        self.fullscreen().is_some() || self.shown_state().fullscreen
    }

    /// What `_NET_WM_STATE` holds of the states the window manager shows the window in, read
    /// once per change of it ([`Self::wm_state_changed`]) rather than per query: a resize asks
    /// several times while the X server is busy with it, each read a round trip; none where it
    /// cannot be read.
    fn shown_state(&self) -> ShownState {
        let changes = {
            let state = self.shared_state_lock();
            if let Some(shown) = state.shown {
                return shown;
            }
            state.shown_changes
        };
        let atoms = self.xconn.atoms();
        let state_atom = atoms[_NET_WM_STATE];
        let Ok(state) = self.xconn.get_property(self.xwindow, state_atom, atom::ATOM) else {
            return ShownState::default();
        };
        let shown = ShownState {
            hidden: state.contains(&atoms[_NET_WM_STATE_HIDDEN]),
            maximized_horz: state.contains(&atoms[_NET_WM_STATE_MAXIMIZED_HORZ]),
            maximized_vert: state.contains(&atoms[_NET_WM_STATE_MAXIMIZED_VERT]),
            fullscreen: state.contains(&atoms[_NET_WM_STATE_FULLSCREEN]),
        };
        let mut cached = self.shared_state_lock();
        if cached.shown_changes == changes {
            cached.shown = Some(shown);
        }
        shown
    }

    /// `_NET_WM_STATE` changed (its `PropertyNotify`): the next query reads it again.
    pub(super) fn wm_state_changed(&self) {
        let mut state = self.shared_state_lock();
        state.shown = None;
        state.shown_changes = state.shown_changes.wrapping_add(1);
    }

    fn set_maximized_inner(&self, maximized: bool) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();
        let horz_atom = atoms[_NET_WM_STATE_MAXIMIZED_HORZ];
        let vert_atom = atoms[_NET_WM_STATE_MAXIMIZED_VERT];

        self.set_netwm(maximized.into(), (horz_atom, vert_atom, 0, 0))
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        let what = "Failed to change window maximization";
        self.set_maximized_inner(maximized).ignore_error_or_report(&self.xconn, what);
        self.xconn.flush_requests().or_report(&self.xconn, what);
        self.invalidate_cached_frame_extents();
    }

    fn set_title_inner(&self, title: &str) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();

        let title = CString::new(title).expect("Window title contained null byte");
        self.xconn
            .change_property(
                self.xwindow,
                atom::WM_NAME,
                atom::STRING,
                xproto::PropMode::Replace,
                title.as_bytes(),
            )?
            .ignore_error();
        self.xconn.change_property(
            self.xwindow,
            atoms[_NET_WM_NAME],
            atoms[UTF8_STRING],
            xproto::PropMode::Replace,
            title.as_bytes(),
        )
    }

    #[inline]
    pub fn set_title(&self, title: &str) {
        let what = "Failed to set window title";
        self.set_title_inner(title).ignore_error_or_report(&self.xconn, what);

        self.xconn.flush_requests().or_report(&self.xconn, what);
    }

    /// Ask the window manager for decorations (Motif hints), as upstream did for every window
    /// created with them (WC.8: no window turns them off).
    fn set_decorations_inner(&self) -> Result<VoidCookie<'_>, X11Error> {
        let mut hints = self.xconn.get_motif_hints(self.xwindow);

        hints.set_decorations(true);

        self.xconn.set_motif_hints(self.xwindow, &hints)
    }

    #[inline]
    pub fn is_decorated(&self) -> bool {
        true
    }

    fn set_icon_inner(&self, icon: PlatformIcon) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.xconn.atoms();
        let icon_atom = atoms[_NET_WM_ICON];
        let data = icon.to_cardinals();
        self.xconn.change_property(
            self.xwindow,
            icon_atom,
            atom::CARDINAL,
            xproto::PropMode::Replace,
            data.as_slice(),
        )
    }

    /// End the launch's startup sequence at the window's first map (the spec's
    /// `remove` comes once the launchee shows its window, so a launcher's busy cursor
    /// or spinner lasts until then).
    fn end_startup_sequence(&self) {
        let Some(startup_id) = self.startup_id.lock().unwrap().take() else {
            return;
        };
        if let Err(_err) = self.xconn.end_startup_sequence(self.xwindow, &startup_id) {
            warn!("Failed to end the startup sequence {startup_id:?}: {_err}");
        }
    }

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        let mut shared_state = self.shared_state_lock();

        match (visible, shared_state.visibility) {
            (true, Visibility::Yes) | (true, Visibility::YesWait) | (false, Visibility::No) => {
                return
            },
            _ => (),
        }

        if visible {
            self.xconn
                .send_void(xproto::map_window(self.xwindow))
                .ignore_error_or_report(&self.xconn, "Failed to call `xcb_map_window`");
            self.xconn
                .send_void(xproto::configure_window(self.xwindow, &stack_above()))
                .ignore_error_or_report(&self.xconn, "Failed to call `xcb_configure_window`");
            self.xconn.flush_requests().or_report(&self.xconn, "Failed to call XMapRaised");
            shared_state.visibility = Visibility::YesWait;
            self.end_startup_sequence();
        } else {
            self.xconn
                .send_void(xproto::unmap_window(self.xwindow))
                .ignore_error_or_report(&self.xconn, "Failed to call `xcb_unmap_window`");
            self.xconn.flush_requests().or_report(&self.xconn, "Failed to call XUnmapWindow");
            shared_state.visibility = Visibility::No;
        }
    }

    /// The window's frame extents: the cached ones, else the heuristic's
    /// (`get_frame_extents_heuristic`, round trips), which are cached.
    fn frame_extents(&self) -> Result<util::FrameExtentsHeuristic, X11Error> {
        let cached = self.shared_state_lock().frame_extents.clone();
        if let Some(extents) = cached {
            return Ok(extents);
        }
        let extents = self.xconn.get_frame_extents_heuristic(self.xwindow, self.root)?;
        self.shared_state_lock().frame_extents = Some(extents.clone());
        Ok(extents)
    }

    pub(crate) fn invalidate_cached_frame_extents(&self) {
        self.shared_state_lock().frame_extents.take();
    }

    /// The frame's top-left corner on the root.
    pub(crate) fn outer_position_physical(&self) -> Result<(i32, i32), X11Error> {
        let extents = self.frame_extents()?;
        let (x, y) = self.inner_position_physical()?;
        Ok(extents.inner_pos_to_outer(x, y))
    }

    /// The frame's top-left corner, or `NotSupportedError` when the server cannot answer (the display gone).
    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        self.outer_position_physical()
            .or_report(&self.xconn, "Failed to get the window's position")
            .map(Into::into)
            .ok_or_else(NotSupportedError::new)
    }

    /// The client area's top-left corner on the root.
    pub(crate) fn inner_position_physical(&self) -> Result<(i32, i32), X11Error> {
        let coords = self.xconn.translate_coords(self.xwindow, self.root)?;
        Ok((coords.dst_x.into(), coords.dst_y.into()))
    }

    /// The client area's top-left corner, or `NotSupportedError` as [`Self::outer_position`].
    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        self.inner_position_physical()
            .or_report(&self.xconn, "Failed to get the window's position")
            .map(Into::into)
            .ok_or_else(NotSupportedError::new)
    }

    pub(crate) fn set_position_inner(
        &self,
        mut x: i32,
        mut y: i32,
    ) -> Result<VoidCookie<'_>, X11Error> {
        // There are a few WMs that set client area position rather than window position, so
        // we'll translate for consistency.
        if util::wm_name_is_one_of(&["Enlightenment", "FVWM"]) {
            let extents = self.frame_extents()?;
            x += cast_dimension_to_hint(extents.frame_extents.left);
            y += cast_dimension_to_hint(extents.frame_extents.top);
        }

        self.xconn.send_void(xproto::configure_window(
            self.xwindow,
            &ValueList::new().set(xproto::config::X, x as u32).set(xproto::config::Y, y as u32),
        ))
    }

    pub(crate) fn set_position_physical(&self, x: i32, y: i32) {
        self.set_position_inner(x, y)
            .ignore_error_or_report(&self.xconn, "Failed to call `XMoveWindow`");
    }

    /// A window not yet mapped also says so in its normal hints (ICCCM USPosition).
    #[inline]
    pub fn set_outer_position(&self, position: Position) {
        let (x, y) = position.to_physical::<i32>(self.scale_factor()).into();
        let mapped = matches!(self.shared_state_lock().visibility, Visibility::Yes);
        if !mapped {
            self.update_normal_hints(|hints| hints.position = Some((Specified::User, x, y)))
                .or_report(&self.xconn, "Failed to call `XSetWMNormalHints`");
        }
        self.set_position_physical(x, y);
    }

    /// The client area's size.
    pub(crate) fn inner_size_physical(&self) -> Result<(u32, u32), X11Error> {
        let geometry = self.xconn.get_geometry(self.xwindow)?;
        Ok((geometry.width.into(), geometry.height.into()))
    }

    /// [`Self::inner_size_physical`], falling back to the last `ConfigureNotify` or creation size.
    pub(crate) fn inner_size_or_last(&self) -> (u32, u32) {
        self.inner_size_physical()
            .or_report(&self.xconn, "Failed to get the window's size")
            .unwrap_or_else(|| self.shared_state_lock().size.unwrap_or(self.created_size))
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        self.inner_size_or_last().into()
    }

    /// The frame's size.
    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        let extents =
            self.frame_extents().or_report(&self.xconn, "Failed to get the window's frame");
        let (width, height) = self.inner_size_or_last();
        match extents {
            Some(extents) => extents.inner_size_to_outer(width, height).into(),
            None => (width, height).into(),
        }
    }

    pub(crate) fn request_inner_size_physical(&self, width: u32, height: u32) {
        self.xconn
            .send_void(xproto::configure_window(
                self.xwindow,
                &ValueList::new()
                    .set(xproto::config::WIDTH, width)
                    .set(xproto::config::HEIGHT, height),
            ))
            .ignore_error_or_report(&self.xconn, "Failed to call `xcb_configure_window`");
        self.xconn.flush_requests().or_report(&self.xconn, "Failed to call XResizeWindow");
    }

    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let scale_factor = self.scale_factor();
        let size: (u32, u32) = size.to_physical::<u32>(scale_factor).into();
        self.request_inner_size_physical(size.0, size.1);

        None
    }

    fn update_normal_hints<F>(&self, callback: F) -> Result<(), X11Error>
    where
        F: FnOnce(&mut WmSizeHints),
    {
        let mut normal_hints = self.xconn.get_normal_hints(self.xwindow)?.unwrap_or_default();
        callback(&mut normal_hints);
        self.xconn.send_void(normal_hints.set(self.xwindow, atom::WM_NORMAL_HINTS))?.ignore_error();
        Ok(())
    }

    pub(crate) fn set_min_inner_size_physical(&self, dimensions: Option<(u32, u32)>) {
        self.update_normal_hints(|normal_hints| {
            normal_hints.min_size =
                dimensions.map(|(w, h)| (cast_dimension_to_hint(w), cast_dimension_to_hint(h)))
        })
        .or_report(&self.xconn, "Failed to call `XSetWMNormalHints`");
    }

    #[inline]
    pub fn set_min_inner_size(&self, dimensions: Option<Size>) {
        self.shared_state_lock().min_inner_size = dimensions;
        let physical_dimensions =
            dimensions.map(|dimensions| dimensions.to_physical::<u32>(self.scale_factor()).into());
        self.set_min_inner_size_physical(physical_dimensions);
    }

    #[inline]
    pub fn set_resize_increments(&self, increments: Option<Size>) {
        self.shared_state_lock().resize_increments = increments;
        let physical_increments =
            increments.map(|increments| cast_size_to_hint(increments, self.scale_factor()));
        self.update_normal_hints(|hints| hints.size_increment = physical_increments)
            .or_report(&self.xconn, "Failed to call `XSetWMNormalHints`");
    }

    pub(crate) fn adjust_for_dpi(
        &self,
        old_scale_factor: f64,
        new_scale_factor: f64,
        width: u32,
        height: u32,
        shared_state: &SharedState,
    ) -> (u32, u32) {
        let scale_factor = new_scale_factor / old_scale_factor;
        self.update_normal_hints(|normal_hints| {
            let dpi_adjuster = |size: Size| -> (i32, i32) { cast_size_to_hint(size, scale_factor) };
            let min_size = shared_state.min_inner_size.map(dpi_adjuster);
            let resize_increments = shared_state.resize_increments.map(dpi_adjuster);

            normal_hints.max_size = None;
            normal_hints.min_size = min_size;
            normal_hints.size_increment = resize_increments;
            normal_hints.base_size = None;
        })
        .or_report(&self.xconn, "Failed to update normal hints");

        let new_width = (width as f64 * scale_factor).round() as u32;
        let new_height = (height as f64 * scale_factor).round() as u32;

        (new_width, new_height)
    }

    /// LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 6, the Linux half).
    #[inline]
    pub fn native_window(&self) -> NativeWindow {
        NativeWindow::Xlib {
            display: NonNull::new(self.xconn.display.cast())
                .expect("X11 display should never be null"),
            screen: self.screen_id,
            window: self.xwindow,
            visual: self.visual,
        }
    }

    #[inline]
    pub fn set_cursor(&self, cursor: Cursor) {
        let Cursor::Icon(icon) = cursor;
        let old_cursor = replace(&mut *self.selected_cursor.lock().unwrap(), icon);
        if icon != old_cursor {
            self.xconn.set_cursor_icon(self.xwindow, icon);
        }
    }

    /// Set the window's cursor again, loaded anew.
    pub(crate) fn reload_cursor(&self) {
        let icon = *self.selected_cursor.lock().unwrap();
        self.xconn.set_cursor_icon(self.xwindow, icon);
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.shared_state_lock().last_monitor.scale_factor
    }

    /// Moves the window while it is being dragged.
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        self.drag_initiate(util::MOVERESIZE_MOVE)
    }

    #[inline]
    pub fn show_window_menu(&self, _position: Position) {}

    /// Resizes the window while it is being dragged.
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        self.drag_initiate(match direction {
            ResizeDirection::East => util::MOVERESIZE_RIGHT,
            ResizeDirection::North => util::MOVERESIZE_TOP,
            ResizeDirection::NorthEast => util::MOVERESIZE_TOPRIGHT,
            ResizeDirection::NorthWest => util::MOVERESIZE_TOPLEFT,
            ResizeDirection::South => util::MOVERESIZE_BOTTOM,
            ResizeDirection::SouthEast => util::MOVERESIZE_BOTTOMRIGHT,
            ResizeDirection::SouthWest => util::MOVERESIZE_BOTTOMLEFT,
            ResizeDirection::West => util::MOVERESIZE_LEFT,
        })
    }

    /// Initiates a drag operation while the left mouse button is pressed.
    fn drag_initiate(&self, action: isize) -> Result<(), ExternalError> {
        let pointer = self
            .xconn
            .query_pointer(self.xwindow, util::VIRTUAL_CORE_POINTER)
            .map_err(|err| ExternalError::Os(os_error!(OsError::XError(err.into()))))?;

        let window = self.inner_position().map_err(ExternalError::NotSupported)?;

        let atoms = self.xconn.atoms();
        let message = atoms[_NET_WM_MOVERESIZE];

        self.xconn
            .send_void(xproto::ungrab_pointer(CURRENT_TIME))
            .map_err(|err| ExternalError::Os(os_error!(OsError::XError(err.into()))))?
            .ignore_error();
        self.xconn.flush_requests().map_err(|err| {
            ExternalError::Os(os_error!(OsError::XError(X11Error::Xlib(err).into())))
        })?;

        self.xconn
            .send_client_msg(
                self.xwindow,
                self.root,
                message,
                Some(
                    xproto::event_mask::SUBSTRUCTURE_REDIRECT
                        | xproto::event_mask::SUBSTRUCTURE_NOTIFY,
                ),
                [
                    (window.x + xinput_fp1616_to_float(pointer.win_x) as i32) as u32,
                    (window.y + xinput_fp1616_to_float(pointer.win_y) as i32) as u32,
                    action.try_into().unwrap(),
                    1,
                    1,
                ],
            )
            .map_err(|err| ExternalError::Os(os_error!(OsError::XError(err.into()))))?;

        self.xconn.flush_requests().map_err(|err| {
            ExternalError::Os(os_error!(OsError::XError(X11Error::Xlib(err).into())))
        })
    }

    #[inline]
    pub fn set_ime_cursor_area(&self, spot: Position, size: Size) {
        let scale = self.scale_factor();
        let (x, y) = xim_spot(spot.to_physical(scale), size.to_physical(scale));
        let _ = self.ime_sender.lock().unwrap().send(ImeRequest::Position(
            self.xwindow as ffi::Window,
            x,
            y,
        ));
    }

    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        let _ = self
            .ime_sender
            .lock()
            .unwrap()
            .send(ImeRequest::Allow(self.xwindow as ffi::Window, allowed));
    }

    #[inline]
    pub fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    #[inline]
    pub fn focus_window(&self) {
        self.focus_window_at(CURRENT_TIME);
    }

    /// [`Self::focus_window`] for an activation another client handed over with `startup_id` (a notification server's click).
    pub fn focus_window_with_activation_token(&self, startup_id: &str) {
        self.focus_window_at(
            super::startup_notify::startup_time(startup_id).unwrap_or(CURRENT_TIME),
        );
        if let Err(_err) = self.xconn.end_startup_sequence(self.xwindow, startup_id) {
            warn!("Failed to end the startup sequence {startup_id:?}: {_err}");
        }
    }

    /// Ask the window manager to raise and focus the window (`_NET_ACTIVE_WINDOW`, as an
    /// application) for the user action at `time` (`CURRENT_TIME` when none is known).
    fn focus_window_at(&self, time: u32) {
        let atoms = self.xconn.atoms();
        let state_atom = atoms[WM_STATE];
        let state_type_atom = atoms[CARD32];
        let is_minimized = if let Ok(state) =
            self.xconn.get_property::<u32>(self.xwindow, state_atom, state_type_atom)
        {
            state.contains(&super::ICONIC_STATE)
        } else {
            false
        };
        let is_visible = match self.shared_state_lock().visibility {
            Visibility::Yes => true,
            Visibility::YesWait | Visibility::No => false,
        };

        if is_visible && !is_minimized {
            self.xconn
                .send_client_msg(
                    self.xwindow,
                    self.root,
                    atoms[_NET_ACTIVE_WINDOW],
                    Some(
                        xproto::event_mask::SUBSTRUCTURE_REDIRECT
                            | xproto::event_mask::SUBSTRUCTURE_NOTIFY,
                    ),
                    [1, time, 0, 0, 0],
                )
                .ignore_error_or_report(&self.xconn, "Failed to send client message");
            if let Err(_e) = self.xconn.flush_requests() {
                tracing::error!(
                    "`flush` returned an error when focusing the window. Error was: {}",
                    _e
                );
            }
        }
    }

    /// Set or clear the urgency hint.
    #[inline]
    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        if request_type.is_some() && self.has_focus() {
            return;
        }
        self.set_urgent(request_type.is_some());
    }

    /// Ring the bell for this window.
    pub fn ring_bell(&self) {
        // SAFETY: the connection's live `Display`; the window is this one's, alive while `self`
        // is; percent 0 is the bell's base volume and name `None` the default bell.
        unsafe {
            (self.xconn.xlib.XkbBell)(self.xconn.display, self.xwindow as ffi::Window, 0, 0);
            (self.xconn.xlib.XFlush)(self.xconn.display);
        }
    }

    /// The window gained focus.
    pub(crate) fn focused(&self) {
        if self.shared_state_lock().urgent {
            self.set_urgent(false);
        }
    }

    fn set_urgent(&self, urgent: bool) {
        let mut wm_hints = self.xconn.get_wm_hints(self.xwindow).ok().flatten().unwrap_or_default();

        wm_hints.urgent = urgent;
        self.xconn
            .send_void(wm_hints.set(self.xwindow))
            .ignore_error_or_report(&self.xconn, "Failed to set WM hints");
        self.shared_state_lock().urgent = urgent;
    }

    #[inline]
    pub fn id(&self) -> WindowId {
        WindowId(self.xwindow as _)
    }

    #[inline]
    pub fn request_redraw(&self) {
        self.redraw_sender.send(WindowId(self.xwindow as _)).unwrap();
    }

    /// Whether the server asked for the window to be drawn again since the last look ([`SharedState::exposed`]), which this clears.
    pub(crate) fn take_exposed(&self) -> bool {
        std::mem::take(&mut self.shared_state_lock().exposed)
    }

    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        None
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        self.shared_state_lock().has_focus
    }
}

/// Cast a dimension value into a hinted dimension for `WmSizeHints`, clamping if too large.
fn cast_dimension_to_hint(val: u32) -> i32 {
    val.try_into().unwrap_or(i32::MAX)
}

/// Use the above strategy to cast a physical size into a hinted size.
fn cast_physical_size_to_hint(size: PhysicalSize<u32>) -> (i32, i32) {
    let PhysicalSize { width, height } = size;
    (cast_dimension_to_hint(width), cast_dimension_to_hint(height))
}

/// Use the above strategy to cast a size into a hinted size.
fn cast_size_to_hint(size: Size, scale_factor: f64) -> (i32, i32) {
    match size {
        Size::Physical(size) => cast_physical_size_to_hint(size),
        Size::Logical(size) => size.to_physical::<i32>(scale_factor).into(),
    }
}

/// The XIM spot location (`XNSpotLocation`) of a caret box at `position`, `size` big, in window pixels.
pub(super) fn xim_spot(position: PhysicalPosition<i32>, size: PhysicalSize<u32>) -> (i16, i16) {
    let clamp = |v: i64| v.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
    (clamp(position.x.into()), clamp(i64::from(position.y) + i64::from(size.height)))
}

/// ConfigureWindow's values that raise a window to the top of the stack.
fn stack_above() -> ValueList {
    // `Above`, the first of ConfigureWindow's stack modes.
    ValueList::new().set(xproto::config::STACK_MODE, 0)
}

impl XConnection {
    /// The window's `WM_NORMAL_HINTS`, `None` when it has none.
    fn get_normal_hints(&self, window: xproto::Window) -> Result<Option<WmSizeHints>, X11Error> {
        let reply = self.request(xproto::get_property(
            false,
            window,
            atom::WM_NORMAL_HINTS,
            atom::WM_SIZE_HINTS,
            0,
            WM_SIZE_HINTS_ITEMS,
        ))?;
        Ok(WmSizeHints::from_reply(&reply).map_err(aterm_x11::Error::from)?)
    }

    /// The window's `WM_HINTS`, `None` when it has none.
    fn get_wm_hints(&self, window: xproto::Window) -> Result<Option<WmHints>, X11Error> {
        let reply = self.request(xproto::get_property(
            false,
            window,
            atom::WM_HINTS,
            atom::WM_HINTS,
            0,
            WM_HINTS_ITEMS,
        ))?;
        Ok(WmHints::from_reply(&reply).map_err(aterm_x11::Error::from)?)
    }
}
