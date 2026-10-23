// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/event_processor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::os::raw::{c_char, c_int, c_long, c_ulong};
use std::slice;
use std::sync::{Arc, Mutex};

use aterm_x11::proto::event::ClientMessage;
use aterm_x11::proto::xproto::{self, QueryExtensionReply};
use aterm_x11::proto::{atom, xinput};
use aterm_xkb::ModMask as xkb_mod_mask_t;

use super::ffi::{
    self, event_type, xi, xkb_event, Display as XDisplay, Window as XWindow, XClientMessageEvent,
    XConfigureEvent, XDestroyWindowEvent, XEvent, XExposeEvent, XIDeviceEvent, XIEnterEvent,
    XIFocusInEvent, XIFocusOutEvent, XIHierarchyEvent, XILeaveEvent, XIModifierState, XKeyEvent,
    XMapEvent, XPropertyEvent, XReparentEvent, XSelectionEvent, XVisibilityEvent, XkbAnyEvent,
    XkbStateRec,
};

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::event::{
    ElementState, Event, Ime, InnerSizeWriter, MouseButton, MouseScrollDelta, Touch, TouchPhase,
    WindowEvent,
};
use crate::event_loop::ActiveEventLoop as RootAEL;
use crate::keyboard::ModifiersState;
use crate::platform_impl::common::xkb::XkbState;
use crate::platform_impl::platform::common::xkb::Context;
use crate::platform_impl::platform::x11::ime::{ImeEvent, ImeEventReceiver, ImeRequest};
use crate::platform_impl::platform::x11::ActiveEventLoop;
use crate::platform_impl::platform::ActiveEventLoop as PlatformActiveEventLoop;
use crate::platform_impl::x11::atoms::*;
use crate::platform_impl::x11::util::cookie::GenericEventCookie;
use crate::platform_impl::x11::window::Exposure;
use crate::platform_impl::x11::{
    mkdid, mkwid, util, CookieResultExt, Device, DeviceId, DeviceInfo, Dnd, DndState, ImeReceiver,
    ReportExt, ScrollOrientation, UnownedWindow, WindowId,
};

// LOCAL PATCH (aterm): events are read through aterm-x11's Xlib and XInput 2 declarations and the
// requests are its encoders, where upstream used x11-dl and x11rb. No device events: the raw XInput
// 2 events are not selected on the root, and `WindowEvent::AxisMotion` is not reported. A window
// manager's `_NET_WM_SYNC_REQUEST` is noted on its window (`sync_request.rs`). A failed request is
// reported (`XConnection::report`), where upstream's `expect`s panicked.

/// The core protocol's modifier bits in a key event's state (`KeyButMask`).
mod mod_mask {
    pub const SHIFT: u16 = 1 << 0;
    pub const LOCK: u16 = 1 << 1;
    pub const CONTROL: u16 = 1 << 2;
    pub const M1: u16 = 1 << 3;
    pub const M2: u16 = 1 << 4;
    pub const M3: u16 = 1 << 5;
    pub const M4: u16 = 1 << 6;
    pub const M5: u16 = 1 << 7;
}

/// The core pointer buttons (`X11/X.h`).
const BUTTON1: u32 = 1;
const BUTTON2: u32 = 2;
const BUTTON3: u32 = 3;

/// `XIPointerEmulated`, a device event's flag for a pointer event emulated from touch.
const XI_POINTER_EMULATED: c_int = 1 << 16;

/// `XIMaskIsSet`: whether bit `bit` of an XI2 mask is set.
fn xi_mask_is_set(mask: &[u8], bit: c_int) -> bool {
    usize::try_from(bit)
        .ok()
        .and_then(|bit| mask.get(bit >> 3).map(|byte| byte & (1 << (bit & 7)) != 0))
        .unwrap_or(false)
}

/// The maximum amount of X modifiers to replay.
pub const MAX_MOD_REPLAY_LEN: usize = 32;

/// X11 keycodes lie in the inclusive range `[8, 255]`.
const KEYCODE_OFFSET: u8 = 8;

pub struct EventProcessor {
    pub dnd: Dnd,
    pub ime_receiver: ImeReceiver,
    pub ime_event_receiver: ImeEventReceiver,
    pub randr_event_offset: u8,
    pub devices: RefCell<HashMap<DeviceId, Device>>,
    pub xi2ext: QueryExtensionReply,
    pub xkbext: QueryExtensionReply,
    pub target: RootAEL,
    pub xkb_context: Context,
    pub num_touch: u32,
    pub held_key_press: Option<u32>,
    pub first_touch: Option<u64>,
    pub active_window: Option<xproto::Window>,
    /// Latest modifiers we've sent for the user to trigger change in event.
    pub modifiers: Cell<ModifiersState>,
    pub xfiltered_modifiers: VecDeque<u8>,
    pub xmodmap: util::ModifierKeymap,
    /// The window whose input context has a composition open.
    pub composing: Option<xproto::Window>,
}

impl EventProcessor {
    pub fn process_event<T: 'static, F>(&mut self, xev: &mut XEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        self.process_xevent(xev, &mut callback);
        self.process_ime(&mut callback);
    }

    /// Send the IME requests the windows queued.
    pub(super) fn process_ime<T: 'static, F>(&mut self, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window_target = Self::window_target_mut(&mut self.target);

        while !window_target.x_connection().is_broken() {
            let Ok(request) = self.ime_receiver.try_recv() else {
                break;
            };
            let ime = match window_target.ime.as_mut() {
                Some(ime) => ime,
                None => continue,
            };
            let ime = ime.get_mut();
            match request {
                ImeRequest::Position(window_id, x, y) => {
                    ime.send_xim_spot(window_id, x, y);
                },
                ImeRequest::Allow(window_id, allowed) => {
                    ime.set_ime_allowed(window_id, allowed);
                },
            }
        }

        while let Ok((window, event)) = self.ime_event_receiver.try_recv() {
            let window = window as xproto::Window;
            let window_id = mkwid(window);
            let event = match event {
                ImeEvent::Enabled => {
                    self.end_composition(window);
                    WindowEvent::Ime(Ime::Enabled)
                },
                ImeEvent::Start => {
                    self.composing = Some(window);
                    WindowEvent::Ime(Ime::Preedit("".to_owned(), None))
                },
                ImeEvent::Update(text, position) if self.composing == Some(window) => {
                    WindowEvent::Ime(Ime::Preedit(text, Some((position, position))))
                },
                ImeEvent::End => {
                    self.end_composition(window);
                    WindowEvent::Ime(Ime::Preedit(String::new(), None))
                },
                ImeEvent::Disabled => {
                    self.end_composition(window);
                    WindowEvent::Ime(Ime::Disabled)
                },
                _ => continue,
            };

            callback(&self.target, Event::WindowEvent { window_id, event });
        }
    }

    /// End `window`'s composition, if it has one open ([`Self::composing`]).
    fn end_composition(&mut self, window: xproto::Window) -> bool {
        let open = self.composing == Some(window);
        if open {
            self.composing = None;
        }
        open
    }

    /// XFilterEvent tells us when an event has been discarded by the input method.
    #[must_use]
    fn filter_event(&mut self, xev: &mut XEvent) -> bool {
        let wt = Self::window_target(&self.target);
        unsafe {
            let window = xev.any.window;
            (wt.xconn.xlib.XFilterEvent)(xev, window) == ffi::TRUE
        }
    }

    fn process_xevent<T: 'static, F>(&mut self, xev: &mut XEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let event_type = unsafe { xev.type_ };

        let filtered =
            if event_type == event_type::KEY_PRESS || event_type == event_type::KEY_RELEASE {
                let wt = Self::window_target(&self.target);
                let ime = wt.ime.as_ref();
                let window = self.active_window.map(|window| window as XWindow);
                let forward_to_ime = ime
                    .and_then(|ime| window.map(|window| ime.borrow().is_ime_allowed(window)))
                    .unwrap_or(false);

                let filtered = forward_to_ime && self.filter_event(xev);
                if filtered {
                    let xev: &XKeyEvent = unsafe { &xev.key };
                    if self.xmodmap.is_modifier(xev.keycode as u8) {
                        // Don't grow the buffer past the `MAX_MOD_REPLAY_LEN`. This could happen
                        // when the modifiers are consumed entirely.
                        if self.xfiltered_modifiers.len() == MAX_MOD_REPLAY_LEN {
                            self.xfiltered_modifiers.pop_back();
                        }
                        self.xfiltered_modifiers.push_front(xev.keycode as u8);
                    }
                }

                filtered
            } else {
                self.filter_event(xev)
            };

        if filtered {
            return;
        }

        // SAFETY: Xlib wrote the member that `event_type` names.
        match event_type {
            event_type::CLIENT_MESSAGE => {
                self.client_message(unsafe { &xev.client_message }, &mut callback)
            },
            event_type::SELECTION_NOTIFY => {
                self.selection_notify(unsafe { &xev.selection }, &mut callback)
            },
            event_type::CONFIGURE_NOTIFY => {
                self.configure_notify(unsafe { &xev.configure }, &mut callback)
            },
            event_type::REPARENT_NOTIFY => self.reparent_notify(unsafe { &xev.reparent }),
            event_type::MAP_NOTIFY => self.map_notify(unsafe { &xev.map }, &mut callback),
            event_type::UNMAP_NOTIFY => self.unmap_notify(unsafe { &xev.unmap }, &mut callback),
            event_type::DESTROY_NOTIFY => {
                self.destroy_notify(unsafe { &xev.destroy_window }, &mut callback)
            },
            event_type::PROPERTY_NOTIFY => {
                self.property_notify(unsafe { &xev.property }, &mut callback)
            },
            event_type::VISIBILITY_NOTIFY => {
                self.visibility_notify(unsafe { &xev.visibility }, &mut callback)
            },
            event_type::EXPOSE => self.expose(unsafe { &xev.expose }, &mut callback),
            ty @ event_type::KEY_PRESS | ty @ event_type::KEY_RELEASE => {
                let state = if ty == event_type::KEY_PRESS {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                };

                self.xinput_key_input(unsafe { &mut xev.key }, state, &mut callback);
            },
            event_type::GENERIC_EVENT => {
                let wt = Self::window_target(&self.target);
                let xev: GenericEventCookie =
                    match GenericEventCookie::from_event(wt.xconn.clone(), *xev) {
                        Some(xev) if xev.extension() == self.xi2ext.major_opcode => xev,
                        _ => return,
                    };

                let evtype = xev.evtype();

                match evtype {
                    ty @ xi::BUTTON_PRESS | ty @ xi::BUTTON_RELEASE => {
                        let state = if ty == xi::BUTTON_PRESS {
                            ElementState::Pressed
                        } else {
                            ElementState::Released
                        };

                        let xev: &XIDeviceEvent = unsafe { xev.as_event() };
                        self.update_mods_from_xinput2_event(
                            &xev.mods,
                            &xev.group,
                            false,
                            &mut callback,
                        );
                        self.xinput2_button_input(xev, state, &mut callback);
                    },
                    xi::MOTION => {
                        let xev: &XIDeviceEvent = unsafe { xev.as_event() };
                        self.update_mods_from_xinput2_event(
                            &xev.mods,
                            &xev.group,
                            false,
                            &mut callback,
                        );
                        self.xinput2_mouse_motion(xev, &mut callback);
                    },
                    xi::ENTER => {
                        let xev: &XIEnterEvent = unsafe { xev.as_event() };
                        self.xinput2_mouse_enter(xev, &mut callback);
                    },
                    xi::LEAVE => {
                        let xev: &XILeaveEvent = unsafe { xev.as_event() };
                        self.update_mods_from_xinput2_event(
                            &xev.mods,
                            &xev.group,
                            false,
                            &mut callback,
                        );
                        self.xinput2_mouse_left(xev, &mut callback);
                    },
                    xi::FOCUS_IN => {
                        let xev: &XIFocusInEvent = unsafe { xev.as_event() };
                        self.xinput2_focused(xev, &mut callback);
                    },
                    xi::FOCUS_OUT => {
                        let xev: &XIFocusOutEvent = unsafe { xev.as_event() };
                        self.xinput2_unfocused(xev, &mut callback);
                    },
                    xi::TOUCH_BEGIN | xi::TOUCH_UPDATE | xi::TOUCH_END => {
                        let phase = match evtype {
                            xi::TOUCH_BEGIN => TouchPhase::Started,
                            xi::TOUCH_UPDATE => TouchPhase::Moved,
                            xi::TOUCH_END => TouchPhase::Ended,
                            _ => unreachable!(),
                        };

                        let xev: &XIDeviceEvent = unsafe { xev.as_event() };
                        self.xinput2_touch(xev, phase, &mut callback);
                    },
                    xi::HIERARCHY_CHANGED => {
                        let xev: &XIHierarchyEvent = unsafe { xev.as_event() };
                        self.xinput2_hierarchy_changed(xev);
                    },
                    _ => {},
                }
            },
            _ => {
                if event_type == self.xkbext.first_event as _ {
                    // SAFETY: an XKB event; `XEvent`'s padding covers every XKB event struct.
                    let xev: &XkbAnyEvent =
                        unsafe { &*(xev as *const XEvent).cast::<XkbAnyEvent>() };
                    self.xkb_event(xev, &mut callback);
                }
                if event_type == self.randr_event_offset as c_int {
                    self.process_dpi_change(&mut callback);
                }
            },
        }
    }

    pub fn poll(&self) -> bool {
        let window_target = Self::window_target(&self.target);
        let result = unsafe { (window_target.xconn.xlib.XPending)(window_target.xconn.display) };

        result != 0
    }

    pub unsafe fn poll_one_event(&mut self, event_ptr: *mut XEvent) -> bool {
        let window_target = Self::window_target(&self.target);
        unsafe extern "C" fn predicate(
            _display: *mut XDisplay,
            _event: *mut XEvent,
            _arg: *mut c_char,
        ) -> c_int {
            1
        }

        let result = unsafe {
            (window_target.xconn.xlib.XCheckIfEvent)(
                window_target.xconn.display,
                event_ptr,
                Some(predicate),
                std::ptr::null_mut(),
            )
        };

        result != 0
    }

    pub fn init_device(&self, device: xinput::DeviceId) {
        let window_target = Self::window_target(&self.target);
        let mut devices = self.devices.borrow_mut();
        if let Some(info) = DeviceInfo::get(&window_target.xconn, device as _) {
            for info in info.iter() {
                devices.insert(DeviceId(info.deviceid as _), Device::new(info));
            }
        }
    }

    pub fn with_window<F, Ret>(&self, window_id: xproto::Window, callback: F) -> Option<Ret>
    where
        F: Fn(&Arc<UnownedWindow>) -> Ret,
    {
        let mut deleted = false;
        let window_id = WindowId(window_id as _);
        let window_target = Self::window_target(&self.target);
        let result = window_target
            .windows
            .borrow()
            .get(&window_id)
            .and_then(|window| {
                let arc = window.upgrade();
                deleted = arc.is_none();
                arc
            })
            .map(|window| callback(&window));

        if deleted {
            window_target.windows.borrow_mut().remove(&window_id);
        }

        result
    }

    /// Get the platform window target.
    pub fn window_target(window_target: &RootAEL) -> &ActiveEventLoop {
        match &window_target.p {
            PlatformActiveEventLoop::X(target) => target,
            _ => unreachable!(),
        }
    }

    /// Get the platform window target.
    pub fn window_target_mut(window_target: &mut RootAEL) -> &mut ActiveEventLoop {
        match &mut window_target.p {
            PlatformActiveEventLoop::X(target) => target,
            _ => unreachable!(),
        }
    }

    fn client_message<T: 'static, F>(&mut self, xev: &XClientMessageEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        let atoms = wt.xconn.atoms();

        let window = xev.window as xproto::Window;
        let window_id = mkwid(window);

        if client_long(xev, 0) as xproto::Atom == wt.wm_delete_window {
            let event = Event::WindowEvent { window_id, event: WindowEvent::CloseRequested };
            callback(&self.target, event);
            return;
        }

        if xev.message_type == atoms[WM_PROTOCOLS] as c_ulong
            && client_long(xev, 0) as xproto::Atom == atoms[_NET_WM_SYNC_REQUEST]
        {
            let low = u64::from(client_long(xev, 2) as u32);
            let high = u64::from(client_long(xev, 3) as u32);
            let value = ((high << 32) | low).cast_signed();
            self.with_window(window, |window| window.sync_requested(value));
            return;
        }

        if client_long(xev, 0) as xproto::Atom == wt.net_wm_ping {
            let mut client_msg = ClientMessage::new32(
                wt.root,
                xev.message_type as _,
                std::array::from_fn(|i| client_long(xev, i) as u32),
            );
            client_msg.format = xev.format as _;
            client_msg.sequence = xev.serial as _;

            wt.xconn
                .send_void(xproto::send_event(
                    false,
                    wt.root,
                    xproto::event_mask::SUBSTRUCTURE_NOTIFY
                        | xproto::event_mask::SUBSTRUCTURE_REDIRECT,
                    &client_msg.to_bytes(),
                ))
                .ignore_error_or_report(&wt.xconn, "Failed to send `ClientMessage` event.");
            return;
        }

        if xev.message_type == atoms[XdndEnter] as c_ulong {
            if self.with_window(window, |_| ()).is_none() {
                return;
            }
            let source_window = client_long(xev, 0) as xproto::Window;
            let flags = client_long(xev, 1);
            let version = flags >> 24;
            self.dnd.reset();
            self.dnd.target_window = Some(window);
            self.dnd.source_window = Some(source_window);
            self.dnd.version = Some(version);
            let has_more_types = flags - (flags & (c_long::MAX - 1)) == 1;
            if !has_more_types {
                let type_list = vec![
                    client_long(xev, 2) as xproto::Atom,
                    client_long(xev, 3) as xproto::Atom,
                    client_long(xev, 4) as xproto::Atom,
                ];
                self.dnd.type_list = Some(type_list);
            } else if let Ok(more_types) = unsafe { self.dnd.get_type_list(source_window) } {
                self.dnd.type_list = Some(more_types);
            }
            return;
        }

        if xev.message_type == atoms[XdndPosition] as c_ulong {
            let source_window = client_long(xev, 0) as xproto::Window;
            if !self.dnd.matches_drag(window, source_window) {
                return;
            }

            let version = self.dnd.version.unwrap_or(5);

            let accepted = if let Some(ref type_list) = self.dnd.type_list {
                type_list.contains(&atoms[TextUriList])
            } else {
                false
            };

            if !accepted {
                unsafe {
                    self.dnd
                        .send_status(window, source_window, DndState::Rejected)
                        .or_report(&wt.xconn, "Failed to send `XdndStatus` message.");
                }
                self.dnd.reset();
                return;
            }

            let time = if version >= 1 {
                client_long(xev, 3) as xproto::Timestamp
            } else {
                aterm_x11::proto::CURRENT_TIME
            };
            self.dnd.request(window, time);

            let state = if self.dnd.refused() { DndState::Rejected } else { DndState::Accepted };
            unsafe {
                self.dnd
                    .send_status(window, source_window, state)
                    .or_report(&wt.xconn, "Failed to send `XdndStatus` message.");
            }
            return;
        }

        if xev.message_type == atoms[XdndDrop] as c_ulong {
            let source = client_long(xev, 0) as xproto::Window;
            if self.dnd.target_window.is_some() && !self.dnd.matches_drag(window, source) {
                return;
            }
            if self.with_window(window, |_| ()).is_none() {
                return;
            }
            if !self.dnd.type_list.as_ref().is_some_and(|types| types.contains(&atoms[TextUriList]))
            {
                self.dnd.result = Some(Vec::new());
            }
            if self.dnd.source_window.is_some() && self.dnd.result.is_none() {
                let time = if self.dnd.version.unwrap_or(5) >= 1 {
                    client_long(xev, 2) as xproto::Timestamp
                } else {
                    aterm_x11::proto::CURRENT_TIME
                };
                self.dnd.request(window, time);
                if self.dnd.result.is_none() {
                    self.dnd.dropped = true;
                    return;
                }
            }
            let (source_window, state) = if let Some(source_window) = self.dnd.source_window {
                let path_list = self.dnd.result.take().unwrap_or_default();
                for path in &path_list {
                    let event = Event::WindowEvent {
                        window_id,
                        event: WindowEvent::DroppedFile(path.clone()),
                    };
                    callback(&self.target, event);
                }
                let state =
                    if path_list.is_empty() { DndState::Rejected } else { DndState::Accepted };
                (source_window, state)
            } else {
                let source_window = client_long(xev, 0) as xproto::Window;
                (source_window, DndState::Rejected)
            };

            unsafe {
                self.dnd
                    .send_finished(window, source_window, state)
                    .or_report(&wt.xconn, "Failed to send `XdndFinished` message.");
            }

            self.dnd.reset();
            return;
        }

        if xev.message_type == atoms[XdndLeave] as c_ulong {
            if !self.dnd.matches_drag(window, client_long(xev, 0) as xproto::Window) {
                return;
            }
            self.dnd.reset();
            let event = Event::WindowEvent { window_id, event: WindowEvent::HoveredFileCancelled };
            callback(&self.target, event);
        }
    }

    fn selection_notify<T: 'static, F>(&mut self, xev: &XSelectionEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        let atoms = wt.xconn.atoms();

        let requestor = xev.requestor as xproto::Window;
        let Some(window) = self.dnd.target_window else {
            return;
        };

        if xev.selection != atoms[XdndSelection] as c_ulong
            || xev.target != atoms[TextUriList] as c_ulong
            || (xev.property != 0 && xev.property != atoms[XdndSelection] as c_ulong)
            || !self.dnd.answers(requestor, xev.time as xproto::Timestamp)
        {
            return;
        }

        let data = if xev.property == atoms[XdndSelection] as c_ulong {
            match self.dnd.take_data(requestor) {
                Some(super::dnd::Answer::List(data)) => data,
                Some(super::dnd::Answer::Incr) => {
                    self.dnd.begin_incr();
                    return;
                },
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        self.dnd_data_came(window, &data, &mut callback);
    }

    /// The drag's data came to `window`.
    fn dnd_data_came<T: 'static, F>(&mut self, window: xproto::Window, data: &[u8], mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        let window_id = mkwid(window);
        self.dnd.complete_request();
        let path_list = super::dnd::local_paths(data);
        if self.dnd.dropped {
            for path in &path_list {
                let event =
                    Event::WindowEvent { window_id, event: WindowEvent::DroppedFile(path.clone()) };
                callback(&self.target, event);
            }
            let state = if path_list.is_empty() { DndState::Rejected } else { DndState::Accepted };
            if let Some(source_window) = self.dnd.source_window {
                unsafe {
                    self.dnd
                        .send_finished(window, source_window, state)
                        .or_report(&wt.xconn, "Failed to send `XdndFinished` message.");
                }
            }
            self.dnd.reset();
            return;
        }
        for path in &path_list {
            let event =
                Event::WindowEvent { window_id, event: WindowEvent::HoveredFile(path.clone()) };
            callback(&self.target, event);
        }
        if path_list.is_empty() {
            // Nothing to drop (a web link, a trash or network item, a refused request): refuse
            // the drag now, not at its next position, so the source's cursor says so while the
            // pointer rests. A source takes an `XdndStatus` at any time (GTK and Qt update the
            // drag's action from it).
            if let Some(source_window) = self.dnd.source_window {
                unsafe {
                    self.dnd
                        .send_status(window, source_window, DndState::Rejected)
                        .or_report(&wt.xconn, "Failed to send `XdndStatus` message.");
                }
            }
        }
        self.dnd.result = Some(path_list);
    }

    fn configure_notify<T: 'static, F>(&self, xev: &XConfigureEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);

        let xwindow = xev.window as xproto::Window;
        let window_id = mkwid(xwindow);

        let window = match self.with_window(xwindow, Arc::clone) {
            Some(window) => window,
            None => return,
        };

        let is_synthetic = xev.send_event == ffi::TRUE;

        let new_inner_size = (xev.width as u32, xev.height as u32);
        let new_inner_position = (xev.x, xev.y);

        let (mut resized, moved) = {
            let mut shared_state_lock = window.shared_state_lock();

            let resized = util::maybe_change(&mut shared_state_lock.size, new_inner_size);
            let moved = if is_synthetic {
                util::maybe_change(&mut shared_state_lock.inner_position, new_inner_position)
            } else {
                let rel_parent = new_inner_position;
                if util::maybe_change(&mut shared_state_lock.inner_position_rel_parent, rel_parent)
                {
                    shared_state_lock.inner_position = None;
                    shared_state_lock.frame_extents = None;
                }
                false
            };
            (resized, moved)
        };

        let position = window.shared_state_lock().position;

        let new_outer_position = if let (Some(position), false) = (position, moved) {
            position
        } else {
            let mut shared_state_lock = window.shared_state_lock();

            let frame_extents =
                shared_state_lock.frame_extents.as_ref().cloned().unwrap_or_else(|| {
                    match wt
                        .xconn
                        .get_frame_extents_heuristic(xwindow, wt.root)
                        .or_report(&wt.xconn, "Failed to get the window's frame")
                    {
                        Some(frame_extents) => {
                            shared_state_lock.frame_extents = Some(frame_extents.clone());
                            frame_extents
                        },
                        None => util::FrameExtentsHeuristic {
                            frame_extents: util::FrameExtents::from_border(0),
                            heuristic_path: util::FrameExtentsHeuristicPath::UnsupportedBordered,
                        },
                    }
                });
            let outer =
                frame_extents.inner_pos_to_outer(new_inner_position.0, new_inner_position.1);
            shared_state_lock.position = Some(outer);

            drop(shared_state_lock);

            if moved {
                callback(&self.target, Event::WindowEvent {
                    window_id,
                    event: WindowEvent::Moved(outer.into()),
                });
            }
            outer
        };

        if is_synthetic {
            let mut shared_state_lock = window.shared_state_lock();
            let (width, height) =
                shared_state_lock.dpi_adjusted.unwrap_or((xev.width as u32, xev.height as u32));

            let last_scale_factor = shared_state_lock.last_monitor.scale_factor;
            let new_scale_factor = {
                let window_rect = util::AaRect::new(new_outer_position, new_inner_size);
                let monitor = wt
                    .xconn
                    .get_monitor_for_window(Some(window_rect))
                    .or_report(&wt.xconn, "Failed to find monitor for window");

                match monitor {
                    None => last_scale_factor,
                    Some(monitor) if monitor.is_dummy() => last_scale_factor,
                    Some(monitor) => {
                        shared_state_lock.last_monitor = monitor.clone();
                        monitor.scale_factor
                    },
                }
            };
            if last_scale_factor != new_scale_factor {
                let (new_width, new_height) = window.adjust_for_dpi(
                    last_scale_factor,
                    new_scale_factor,
                    width,
                    height,
                    &shared_state_lock,
                );

                let old_inner_size = PhysicalSize::new(width, height);
                let new_inner_size = PhysicalSize::new(new_width, new_height);

                drop(shared_state_lock);

                let inner_size = Arc::new(Mutex::new(new_inner_size));
                callback(&self.target, Event::WindowEvent {
                    window_id,
                    event: WindowEvent::ScaleFactorChanged {
                        scale_factor: new_scale_factor,
                        inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&inner_size)),
                    },
                });

                let new_inner_size = *inner_size.lock().unwrap();
                drop(inner_size);

                if new_inner_size != old_inner_size {
                    window.request_inner_size_physical(new_inner_size.width, new_inner_size.height);
                    window.shared_state_lock().dpi_adjusted = Some(new_inner_size.into());
                    resized = true;
                }
            }
        }

        {
            let mut shared_state_lock = window.shared_state_lock();

            if let Some(adjusted_size) = shared_state_lock.dpi_adjusted {
                if new_inner_size == adjusted_size || !util::wm_name_is_one_of(&["Xfwm4"]) {
                    shared_state_lock.dpi_adjusted = None;
                } else {
                    drop(shared_state_lock);
                    window.request_inner_size_physical(adjusted_size.0, adjusted_size.1);
                }
            }
        }

        window.sync_configured(resized);

        if resized {
            callback(&self.target, Event::WindowEvent {
                window_id,
                event: WindowEvent::Resized(new_inner_size.into()),
            });
        }
    }

    /// This is generally a reliable way to detect when the window manager's been replaced.
    fn reparent_notify(&self, xev: &XReparentEvent) {
        let wt = Self::window_target(&self.target);

        wt.xconn.update_cached_wm_info(wt.root);

        self.with_window(xev.window as xproto::Window, |window| {
            window.invalidate_cached_frame_extents();
        });
    }

    fn map_notify<T: 'static, F>(&self, xev: &XMapEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window = xev.window as xproto::Window;
        let window_id = mkwid(window);

        let focus = self.with_window(window, |window| window.has_focus()).unwrap_or_default();
        let event = Event::WindowEvent { window_id, event: WindowEvent::Focused(focus) };

        callback(&self.target, event);

        self.change_occlusion(window, |occlusion| occlusion.unmapped = false, &mut callback);
    }

    /// The window was unmapped.
    fn unmap_notify<T: 'static, F>(&self, xev: &ffi::XUnmapEvent, callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window = xev.window as xproto::Window;
        self.change_occlusion(window, |occlusion| occlusion.unmapped = true, callback);
    }

    /// Applies `change` to what hides `window`.
    fn change_occlusion<T: 'static, F>(
        &self,
        window: xproto::Window,
        change: impl FnOnce(&mut super::window::Occlusion),
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let Some(shown) = self.with_window(window, Arc::clone) else {
            return;
        };
        let Some(occluded) = shown.shared_state_lock().occlusion.change(change) else {
            return;
        };
        let event =
            Event::WindowEvent { window_id: mkwid(window), event: WindowEvent::Occluded(occluded) };
        callback(&self.target, event);
    }

    fn destroy_notify<T: 'static, F>(&mut self, xev: &XDestroyWindowEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);

        let window = xev.window as xproto::Window;
        let window_id = mkwid(window);
        if self.dnd.target_window == Some(window) || self.dnd.is_requestor(window) {
            self.dnd.reset();
        }

        wt.windows.borrow_mut().remove(&WindowId(window as _));

        if let Some(ime) = wt.ime.as_ref() {
            ime.borrow_mut()
                .remove_context(window as XWindow)
                .or_report(&wt.xconn, "Failed to destroy input context");
        }

        callback(&self.target, Event::WindowEvent { window_id, event: WindowEvent::Destroyed });
    }

    pub(super) fn property_notify<T: 'static, F>(&mut self, xev: &XPropertyEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        let atoms = wt.x_connection().atoms();
        let atom = xev.atom as xproto::Atom;

        if atom == atom::RESOURCE_MANAGER || atom == atoms[_XSETTINGS_SETTINGS] {
            self.process_dpi_change(&mut callback);
        }

        if atom == atoms[_NET_WM_STATE] {
            let window = xev.window as xproto::Window;
            if let Some(hidden) = self.with_window(window, |shown| {
                shown.wm_state_changed();
                shown.is_minimized()
            }) {
                let hidden = hidden == Some(true);
                self.change_occlusion(window, |occlusion| occlusion.hidden = hidden, &mut callback);
            }
        }

        let window = xev.window as xproto::Window;
        if atom == atoms[XdndSelection]
            && xev.state == c_int::from(aterm_x11::proto::event::property::NEW_VALUE)
            && self.dnd.takes_chunk(window)
        {
            let chunk = self.dnd.take_chunk(window);
            if let Some(data) = self.dnd.add_chunk(chunk) {
                if let Some(target) = self.dnd.target_window {
                    self.dnd_data_came(target, &data, &mut callback);
                }
            }
        }
    }

    fn visibility_notify<T: 'static, F>(&self, xev: &XVisibilityEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let xwindow = xev.window as xproto::Window;

        let obscured = xev.state == ffi::VISIBILITY_FULLY_OBSCURED;
        self.change_occlusion(xwindow, |occlusion| occlusion.obscured = obscured, &mut callback);

        self.with_window(xwindow, |window| {
            window.visibility_notify();
        });
    }

    fn expose<T: 'static, F>(&self, xev: &XExposeEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window = xev.window as xproto::Window;
        let at = |v: c_int| u32::try_from(v).unwrap_or(0);
        let rect = [
            at(xev.x),
            at(xev.y),
            at(xev.x).saturating_add(at(xev.width)),
            at(xev.y).saturating_add(at(xev.height)),
        ];
        self.with_window(window, |window| {
            let mut state = window.shared_state_lock();
            state.exposed = Some(Exposure::widened(state.exposed, xev.serial, rect));
        });

        // We issue `RedrawRequested` only for the last event of such a series.
        if xev.count == 0 {
            let window_id = mkwid(window);

            let event = Event::WindowEvent { window_id, event: WindowEvent::RedrawRequested };

            callback(&self.target, event);
        }
    }

    pub(super) fn xinput_key_input<T: 'static, F>(
        &mut self,
        xev: &mut XKeyEvent,
        state: ElementState,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window = match self.active_window {
            Some(window) => window,
            None => return,
        };

        let window_id = mkwid(window);
        let device_id = mkdid(util::VIRTUAL_CORE_KEYBOARD);
        if state == ElementState::Pressed {
            crate::platform::x11::note_user_time(xev.time as u32);
        }

        let keycode = xev.keycode as _;

        let key_repeats =
            self.xkb_context.keymap_mut().map(|k| k.key_repeats(keycode)).unwrap_or(false);
        let repeat = if key_repeats {
            let is_latest_held = self.held_key_press == Some(keycode);

            if state == ElementState::Pressed {
                self.held_key_press = Some(keycode);
                is_latest_held
            } else {
                // Check that the released key is the latest repeatable key that has been
                // pressed, since repeats will continue for the latest key press if a
                // different previously pressed key is released.
                if is_latest_held {
                    self.held_key_press = None;
                }
                false
            }
        } else {
            false
        };

        let replay = if let Some(position) =
            self.xfiltered_modifiers.iter().rev().position(|&s| s == xev.keycode as u8)
        {
            self.xfiltered_modifiers.resize(self.xfiltered_modifiers.len() - 1 - position, 0);
            true
        } else {
            false
        };

        if !replay {
            self.update_mods_from_core_event(window_id, xev.state as u16, &mut callback);
        }

        if keycode != 0 && self.composing != Some(window) {
            // Don't alter the modifiers state from replaying.
            if replay {
                self.send_synthic_modifier_from_core(window_id, xev.state as u16, &mut callback);
            }

            if let Some(mut key_processor) = self.xkb_context.key_context() {
                let event = key_processor.process_key_event(keycode, state, repeat);
                let event = Event::WindowEvent {
                    window_id,
                    event: WindowEvent::KeyboardInput { device_id, event, is_synthetic: false },
                };
                let target = &self.target;
                crate::platform::x11::dispatch_with_key_event_server_time(xev.time as u32, || {
                    callback(target, event)
                });
            }

            if replay {
                self.send_modifiers(window_id, self.modifiers.get(), true, &mut callback);
            }

            return;
        }

        let wt = Self::window_target(&self.target);

        if let Some(ic) =
            wt.ime.as_ref().and_then(|ime| ime.borrow().get_context(window as XWindow))
        {
            let written = wt.xconn.lookup_utf8(ic, xev);
            if !written.is_empty() {
                let event = Event::WindowEvent {
                    window_id,
                    event: WindowEvent::Ime(Ime::Preedit(String::new(), None)),
                };
                callback(&self.target, event);

                let event =
                    Event::WindowEvent { window_id, event: WindowEvent::Ime(Ime::Commit(written)) };

                self.end_composition(window);
                callback(&self.target, event);
            }
        }
    }

    fn send_synthic_modifier_from_core<T: 'static, F>(
        &mut self,
        window_id: crate::window::WindowId,
        state: u16,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let keymap = match self.xkb_context.keymap_mut() {
            Some(keymap) => keymap,
            None => return,
        };

        let wt = Self::window_target(&self.target);
        let xcb = wt.xconn.xcb_connection().as_raw().as_ptr();

        let mut xkb_state = match XkbState::new_x11(xcb, keymap) {
            Some(xkb_state) => xkb_state,
            None => return,
        };

        let mask = self.xkb_mod_mask_from_core(state);
        xkb_state.update_modifiers(mask, 0, 0, 0, 0, Self::core_keyboard_group(state));
        let mods: ModifiersState = xkb_state.modifiers().into();

        let event =
            Event::WindowEvent { window_id, event: WindowEvent::ModifiersChanged(mods.into()) };

        callback(&self.target, event);
    }

    pub(super) fn xinput2_button_input<T: 'static, F>(
        &self,
        event: &XIDeviceEvent,
        state: ElementState,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window_id = mkwid(event.event as xproto::Window);
        let device_id = mkdid(event.deviceid as xinput::DeviceId);

        if (event.flags & XI_POINTER_EMULATED) != 0 {
            return;
        }
        if state == ElementState::Pressed {
            crate::platform::x11::note_user_time(event.time as u32);
        }

        let event = match event.detail as u32 {
            BUTTON1 => WindowEvent::MouseInput { device_id, state, button: MouseButton::Left },
            BUTTON2 => WindowEvent::MouseInput { device_id, state, button: MouseButton::Middle },

            BUTTON3 => WindowEvent::MouseInput { device_id, state, button: MouseButton::Right },

            // LOCAL PATCH (aterm): a wheel button with no scroll valuator (XTEST: xdotool, x11vnc,
            // Barrier/Synergy) sends a press and a release per notch; one notch is one step, so the
            // release is dropped.
            4..=7 if state == ElementState::Released => return,
            4..=7 => WindowEvent::MouseWheel {
                device_id,
                delta: match event.detail {
                    4 => MouseScrollDelta::LineDelta(0.0, 1.0),
                    5 => MouseScrollDelta::LineDelta(0.0, -1.0),
                    6 => MouseScrollDelta::LineDelta(1.0, 0.0),
                    7 => MouseScrollDelta::LineDelta(-1.0, 0.0),
                    _ => unreachable!(),
                },
                phase: TouchPhase::Moved,
            },
            8 => WindowEvent::MouseInput { device_id, state, button: MouseButton::Back },

            9 => WindowEvent::MouseInput { device_id, state, button: MouseButton::Forward },
            x => WindowEvent::MouseInput { device_id, state, button: MouseButton::Other(x as u16) },
        };

        let event = Event::WindowEvent { window_id, event };
        callback(&self.target, event);
    }

    fn xinput2_mouse_motion<T: 'static, F>(&self, event: &XIDeviceEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let device_id = mkdid(event.deviceid as xinput::DeviceId);
        let window = event.event as xproto::Window;
        let window_id = mkwid(window);
        let new_cursor_pos = (event.event_x, event.event_y);

        let cursor_moved = self.with_window(window, |window| {
            let mut shared_state_lock = window.shared_state_lock();
            util::maybe_change(&mut shared_state_lock.cursor_pos, new_cursor_pos)
        });

        if cursor_moved == Some(true) {
            let position = PhysicalPosition::new(event.event_x, event.event_y);

            let event = Event::WindowEvent {
                window_id,
                event: WindowEvent::CursorMoved { device_id, position },
            };
            callback(&self.target, event);
        } else if cursor_moved.is_none() {
            return;
        }

        let mask = unsafe {
            slice::from_raw_parts(event.valuators.mask, event.valuators.mask_len as usize)
        };
        let mut devices = self.devices.borrow_mut();
        let physical_device = match devices.get_mut(&DeviceId(event.sourceid as xinput::DeviceId)) {
            Some(device) => device,
            None => return,
        };

        let mut events = Vec::new();
        let mut value = event.valuators.values;
        for i in 0..event.valuators.mask_len * 8 {
            if !xi_mask_is_set(mask, i) {
                continue;
            }

            let x = unsafe { *value };

            if let Some(&mut (_, ref mut info)) =
                physical_device.scroll_axes.iter_mut().find(|&&mut (axis, _)| axis == i as _)
            {
                let delta = (x - info.position) / info.increment;
                info.position = x;
                let delta = match info.orientation {
                    ScrollOrientation::Horizontal => {
                        MouseScrollDelta::LineDelta(-delta as f32, 0.0)
                    },
                    ScrollOrientation::Vertical => MouseScrollDelta::LineDelta(0.0, -delta as f32),
                };

                let event = WindowEvent::MouseWheel { device_id, delta, phase: TouchPhase::Moved };
                events.push(Event::WindowEvent { window_id, event });
            }

            value = unsafe { value.offset(1) };
        }

        for event in events {
            callback(&self.target, event);
        }
    }

    fn xinput2_mouse_enter<T: 'static, F>(&self, event: &XIEnterEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);

        let window = event.event as xproto::Window;
        let window_id = mkwid(window);
        let device_id = mkdid(event.deviceid as xinput::DeviceId);

        if let Some(all_info) = DeviceInfo::get(&wt.xconn, super::ALL_DEVICES.into()) {
            let mut devices = self.devices.borrow_mut();
            for device_info in all_info.iter() {
                // The second expression is need for resetting to work correctly on i3, and
                // presumably some other WMs. On those, `XI_Enter` doesn't include the physical
                // device ID, so both `sourceid` and `deviceid` are the virtual device.
                if device_info.deviceid == event.sourceid
                    || device_info.attachment == event.sourceid
                {
                    let device_id = DeviceId(device_info.deviceid as _);
                    if let Some(device) = devices.get_mut(&device_id) {
                        device.reset_scroll_position(device_info);
                    }
                }
            }
        }

        if self.window_exists(window) {
            let position = PhysicalPosition::new(event.event_x, event.event_y);

            let event =
                Event::WindowEvent { window_id, event: WindowEvent::CursorEntered { device_id } };
            callback(&self.target, event);

            let event = Event::WindowEvent {
                window_id,
                event: WindowEvent::CursorMoved { device_id, position },
            };
            callback(&self.target, event);
        }
    }

    fn xinput2_mouse_left<T: 'static, F>(&self, event: &XILeaveEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window = event.event as xproto::Window;

        if self.window_exists(window) {
            let event = Event::WindowEvent {
                window_id: mkwid(window),
                event: WindowEvent::CursorLeft {
                    device_id: mkdid(event.deviceid as xinput::DeviceId),
                },
            };
            callback(&self.target, event);
        }
    }

    pub(super) fn xinput2_focused<T: 'static, F>(&mut self, xev: &XIFocusInEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        let window = xev.event as xproto::Window;

        if is_grab_focus_change(xev.mode) {
            return;
        }

        if let Some(ime) = wt.ime.as_ref() {
            ime.borrow_mut().focus(xev.event).or_report(&wt.xconn, "Failed to focus input context");
        }

        if self.active_window == Some(window) {
            return;
        }

        self.active_window = Some(window);

        let window_id = mkwid(window);
        let position = PhysicalPosition::new(xev.event_x, xev.event_y);

        if let Some(window) = self.with_window(window, Arc::clone) {
            window.shared_state_lock().has_focus = true;
            window.focused();
        }

        let event = Event::WindowEvent { window_id, event: WindowEvent::Focused(true) };
        callback(&self.target, event);

        Self::handle_pressed_keys(
            &self.target,
            window_id,
            ElementState::Pressed,
            &mut self.xkb_context,
            &mut callback,
        );

        self.update_mods_from_query(window_id, &mut callback);

        let pointer_id = self
            .devices
            .borrow()
            .get(&DeviceId(xev.deviceid as xinput::DeviceId))
            .map(|device| device.attachment)
            .unwrap_or(2);

        let event = Event::WindowEvent {
            window_id,
            event: WindowEvent::CursorMoved { device_id: mkdid(pointer_id as _), position },
        };
        callback(&self.target, event);
    }

    pub(super) fn xinput2_unfocused<T: 'static, F>(
        &mut self,
        xev: &XIFocusOutEvent,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        let window = xev.event as xproto::Window;

        if is_grab_focus_change(xev.mode) {
            return;
        }

        if !self.window_exists(window) {
            return;
        }

        if let Some(ime) = wt.ime.as_ref() {
            ime.borrow_mut()
                .unfocus(xev.event)
                .or_report(&wt.xconn, "Failed to unfocus input context");
        }

        if self.end_composition(window) {
            let event = WindowEvent::Ime(Ime::Preedit(String::new(), None));
            callback(&self.target, Event::WindowEvent { window_id: mkwid(window), event });
        }

        if self.active_window.take() == Some(window) {
            let window_id = mkwid(window);

            if let Some(xkb_state) = self.xkb_context.state_mut() {
                xkb_state.update_modifiers(0, 0, 0, 0, 0, 0);
                let mods = xkb_state.modifiers();
                self.send_modifiers(window_id, mods.into(), true, &mut callback);
            }

            Self::handle_pressed_keys(
                &self.target,
                window_id,
                ElementState::Released,
                &mut self.xkb_context,
                &mut callback,
            );

            self.held_key_press = None;

            if let Some(window) = self.with_window(window, Arc::clone) {
                window.shared_state_lock().has_focus = false;
            }

            let event = Event::WindowEvent { window_id, event: WindowEvent::Focused(false) };
            callback(&self.target, event)
        }
    }

    fn xinput2_touch<T: 'static, F>(
        &mut self,
        xev: &XIDeviceEvent,
        phase: TouchPhase,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let window = xev.event as xproto::Window;
        if self.window_exists(window) {
            let window_id = mkwid(window);
            let id = xev.detail as u64;
            let location = PhysicalPosition::new(xev.event_x, xev.event_y);

            if is_first_touch(&mut self.first_touch, &mut self.num_touch, id, phase) {
                let event = Event::WindowEvent {
                    window_id,
                    event: WindowEvent::CursorMoved {
                        device_id: mkdid(util::VIRTUAL_CORE_POINTER),
                        position: location.cast(),
                    },
                };
                callback(&self.target, event);
            }

            let event = Event::WindowEvent {
                window_id,
                event: WindowEvent::Touch(Touch {
                    device_id: mkdid(xev.deviceid as xinput::DeviceId),
                    phase,
                    location,
                    force: None,
                    id,
                }),
            };
            callback(&self.target, event)
        }
    }

    /// A device came or went.
    fn xinput2_hierarchy_changed(&mut self, xev: &XIHierarchyEvent) {
        let infos = unsafe { slice::from_raw_parts(xev.info, xev.num_info as usize) };
        for info in infos {
            if 0 != info.flags & (xi::SLAVE_ADDED | xi::MASTER_ADDED) {
                self.init_device(info.deviceid as xinput::DeviceId);
            } else if 0 != info.flags & (xi::SLAVE_REMOVED | xi::MASTER_REMOVED) {
                let mut devices = self.devices.borrow_mut();
                devices.remove(&DeviceId(info.deviceid as xinput::DeviceId));
            }
        }
    }

    fn xkb_event<T: 'static, F>(&mut self, xev: &XkbAnyEvent, mut callback: F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        match xev.xkb_type {
            xkb_event::NEW_KEYBOARD_NOTIFY => {
                let xev = unsafe {
                    &*(xev as *const XkbAnyEvent).cast::<ffi::XkbNewKeyboardNotifyEvent>()
                };

                let keycodes_changed_flag = 0x1;
                let geometry_changed_flag = 0x1 << 1;

                let keycodes_changed = util::has_flag(xev.changed, keycodes_changed_flag);
                let geometry_changed = util::has_flag(xev.changed, geometry_changed_flag);

                if xev.device == self.xkb_context.core_keyboard_id
                    && (keycodes_changed || geometry_changed)
                {
                    let xcb = wt.xconn.xcb_connection().as_raw().as_ptr();
                    self.xkb_context.set_keymap_from_x11(xcb);
                    self.xmodmap.reload_from_x_connection(&wt.xconn);

                    let window_id = match self.active_window.map(super::mkwid) {
                        Some(window_id) => window_id,
                        None => return,
                    };

                    if let Some(state) = self.xkb_context.state_mut() {
                        let mods = state.modifiers().into();
                        self.send_modifiers(window_id, mods, true, &mut callback);
                    }
                }
            },
            xkb_event::MAP_NOTIFY => {
                let xcb = wt.xconn.xcb_connection().as_raw().as_ptr();
                self.xkb_context.set_keymap_from_x11(xcb);
                self.xmodmap.reload_from_x_connection(&wt.xconn);
                let window_id = match self.active_window.map(super::mkwid) {
                    Some(window_id) => window_id,
                    None => return,
                };

                if let Some(state) = self.xkb_context.state_mut() {
                    let mods = state.modifiers().into();
                    self.send_modifiers(window_id, mods, true, &mut callback);
                }
            },
            xkb_event::STATE_NOTIFY => {
                let xev =
                    unsafe { &*(xev as *const XkbAnyEvent).cast::<ffi::XkbStateNotifyEvent>() };

                if let Some(state) = self.xkb_context.state_mut() {
                    state.update_modifiers(
                        xev.base_mods,
                        xev.latched_mods,
                        xev.locked_mods,
                        xev.base_group as u32,
                        xev.latched_group as u32,
                        xev.locked_group as u32,
                    );

                    let window_id = match self.active_window.map(super::mkwid) {
                        Some(window_id) => window_id,
                        None => return,
                    };

                    let mods = state.modifiers().into();
                    self.send_modifiers(window_id, mods, true, &mut callback);
                }
            },
            _ => {},
        }
    }

    pub fn update_mods_from_xinput2_event<T: 'static, F>(
        &mut self,
        mods: &XIModifierState,
        group: &XIModifierState,
        force: bool,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        if let Some(state) = self.xkb_context.state_mut() {
            state.update_modifiers(
                mods.base as u32,
                mods.latched as u32,
                mods.locked as u32,
                group.base as u32,
                group.latched as u32,
                group.locked as u32,
            );

            let window_id = match self.active_window.map(super::mkwid) {
                Some(window_id) => window_id,
                None => return,
            };

            let mods = state.modifiers();
            self.send_modifiers(window_id, mods.into(), force, &mut callback);
        }
    }

    fn update_mods_from_query<T: 'static, F>(
        &mut self,
        window_id: crate::window::WindowId,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);

        let xkb_state = match self.xkb_context.state_mut() {
            Some(xkb_state) => xkb_state,
            None => return,
        };

        unsafe {
            let mut state: XkbStateRec = std::mem::zeroed();
            if (wt.xconn.xlib.XkbGetState)(wt.xconn.display, ffi::XKB_USE_CORE_KBD, &mut state)
                == ffi::TRUE
            {
                xkb_state.update_modifiers(
                    state.base_mods as u32,
                    state.latched_mods as u32,
                    state.locked_mods as u32,
                    state.base_group as u32,
                    state.latched_group as u32,
                    state.locked_group as u32,
                );
            }
        }

        let mods = xkb_state.modifiers();
        self.send_modifiers(window_id, mods.into(), true, &mut callback)
    }

    pub fn update_mods_from_core_event<T: 'static, F>(
        &mut self,
        window_id: crate::window::WindowId,
        state: u16,
        mut callback: F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let xkb_mask = self.xkb_mod_mask_from_core(state);
        let xkb_state = match self.xkb_context.state_mut() {
            Some(xkb_state) => xkb_state,
            None => return,
        };

        let mut depressed = xkb_state.depressed_modifiers() & xkb_mask;
        let latched = xkb_state.latched_modifiers() & xkb_mask;
        let locked = xkb_state.locked_modifiers() & xkb_mask;
        depressed |= !(depressed | latched | locked) & xkb_mask;

        xkb_state.update_modifiers(
            depressed,
            latched,
            locked,
            0,
            0,
            Self::core_keyboard_group(state),
        );

        let mods = xkb_state.modifiers();
        self.send_modifiers(window_id, mods.into(), false, &mut callback);
    }

    pub fn core_keyboard_group(state: u16) -> u32 {
        ((state >> 13) & 3) as u32
    }

    pub fn xkb_mod_mask_from_core(&mut self, state: u16) -> xkb_mod_mask_t {
        let mods_indices = match self.xkb_context.keymap_mut() {
            Some(keymap) => keymap.mods_indices(),
            None => return 0,
        };

        let mut depressed = 0u32;
        if let Some(shift) = mods_indices.shift.filter(|_| state & mod_mask::SHIFT != 0) {
            depressed |= 1 << shift;
        }
        if let Some(caps) = mods_indices.caps.filter(|_| state & mod_mask::LOCK != 0) {
            depressed |= 1 << caps;
        }
        if let Some(ctrl) = mods_indices.ctrl.filter(|_| state & mod_mask::CONTROL != 0) {
            depressed |= 1 << ctrl;
        }
        if let Some(alt) = mods_indices.alt.filter(|_| state & mod_mask::M1 != 0) {
            depressed |= 1 << alt;
        }
        if let Some(num) = mods_indices.num.filter(|_| state & mod_mask::M2 != 0) {
            depressed |= 1 << num;
        }
        if let Some(mod3) = mods_indices.mod3.filter(|_| state & mod_mask::M3 != 0) {
            depressed |= 1 << mod3;
        }
        if let Some(logo) = mods_indices.logo.filter(|_| state & mod_mask::M4 != 0) {
            depressed |= 1 << logo;
        }
        if let Some(mod5) = mods_indices.mod5.filter(|_| state & mod_mask::M5 != 0) {
            depressed |= 1 << mod5;
        }

        depressed
    }

    /// Send modifiers for the active window.
    fn send_modifiers<T: 'static, F: FnMut(&RootAEL, Event<T>)>(
        &self,
        window_id: crate::window::WindowId,
        modifiers: ModifiersState,
        force: bool,
        callback: &mut F,
    ) {
        // NOTE: Always update the modifiers to account for case when they've changed
        // and forced was `true`.
        if self.modifiers.replace(modifiers) != modifiers || force {
            let event = Event::WindowEvent {
                window_id,
                event: WindowEvent::ModifiersChanged(self.modifiers.get().into()),
            };
            callback(&self.target, event);
        }
    }

    fn handle_pressed_keys<T: 'static, F>(
        target: &RootAEL,
        window_id: crate::window::WindowId,
        state: ElementState,
        xkb_context: &mut Context,
        callback: &mut F,
    ) where
        F: FnMut(&RootAEL, Event<T>),
    {
        let device_id = mkdid(util::VIRTUAL_CORE_KEYBOARD);

        let window_target = Self::window_target(target);
        let xcb = window_target.xconn.xcb_connection().as_raw().as_ptr();

        let keymap = match xkb_context.keymap_mut() {
            Some(keymap) => keymap,
            None => return,
        };

        let mut xkb_state = match XkbState::new_x11(xcb, keymap) {
            Some(xkb_state) => xkb_state,
            None => return,
        };
        let mut key_processor = match xkb_context.key_context_with_state(&mut xkb_state) {
            Some(key_processor) => key_processor,
            None => return,
        };

        for keycode in
            window_target.xconn.query_keymap().into_iter().filter(|k| *k >= KEYCODE_OFFSET)
        {
            let event = key_processor.process_key_event(keycode as u32, state, false);
            let event = Event::WindowEvent {
                window_id,
                event: WindowEvent::KeyboardInput { device_id, event, is_synthetic: true },
            };
            callback(target, event);
        }
    }

    fn process_dpi_change<T: 'static, F>(&self, callback: &mut F)
    where
        F: FnMut(&RootAEL, Event<T>),
    {
        let wt = Self::window_target(&self.target);
        wt.xconn.reload_database().or_report(&wt.xconn, "failed to reload Xft database");
        if wt.xconn.follow_cursor_settings() {
            for window in wt.windows.borrow().values().filter_map(|w| w.upgrade()) {
                window.reload_cursor();
            }
        }

        let prev_list = {
            let prev_list = wt.xconn.invalidate_cached_monitor_list();
            match prev_list {
                Some(prev_list) => prev_list,
                None => return,
            }
        };

        let Some(new_list) =
            wt.xconn.available_monitors().or_report(&wt.xconn, "Failed to get monitor list")
        else {
            return;
        };
        for new_monitor in new_list {
            let maybe_prev_scale_factor = prev_list
                .iter()
                .find(|prev_monitor| prev_monitor.name == new_monitor.name)
                .map(|prev_monitor| prev_monitor.scale_factor);
            if Some(new_monitor.scale_factor) != maybe_prev_scale_factor {
                for window in wt.windows.borrow().values().filter_map(|w| w.upgrade()) {
                    window.refresh_dpi_for_monitor(&new_monitor, maybe_prev_scale_factor, |event| {
                        callback(&self.target, event);
                    })
                }
            }
        }
    }

    fn window_exists(&self, window_id: xproto::Window) -> bool {
        self.with_window(window_id, |_| ()).is_some()
    }
}

fn is_first_touch(first: &mut Option<u64>, num: &mut u32, id: u64, phase: TouchPhase) -> bool {
    match phase {
        TouchPhase::Started => {
            if *num == 0 {
                *first = Some(id);
            }
            *num += 1;
        },
        TouchPhase::Cancelled | TouchPhase::Ended => {
            if *first == Some(id) {
                *first = None;
            }
            *num = num.saturating_sub(1);
        },
        _ => (),
    }

    *first == Some(id)
}

/// The `i`th of a format-32 client message's five values.
fn client_long(xev: &XClientMessageEvent, i: usize) -> c_long {
    // SAFETY: every member of the data union is plain bytes.
    unsafe { xev.data.l[i] }
}

/// Whether an XI2 focus event's `mode` is a keyboard grab's start or end.
fn is_grab_focus_change(mode: c_int) -> bool {
    /// XI2 focus modes (XI2.h).
    const GRAB: c_int = 1;
    const UNGRAB: c_int = 2;
    const PASSIVE_GRAB: c_int = 4;
    const PASSIVE_UNGRAB: c_int = 5;
    matches!(mode, GRAB | UNGRAB | PASSIVE_GRAB | PASSIVE_UNGRAB)
}
