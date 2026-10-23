// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wayland seat binding (`wl_seat` <= v7) and pointer, keyboard, and touch event decoding.

use std::ffi::CStr;
use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crate::globals::Registry;
use crate::pointer_frame::{Frames, PointerEvent, Raw};
use crate::protocols::wayland::{wl_keyboard, wl_pointer, wl_seat, wl_touch};
use crate::proxy::{Error, EventArg, Fixed, Listener, Proxy};
use crate::sink::Sink;

/// The highest `wl_seat` version bound (smithay-client-toolkit 0.19.2's).
pub const VERSION: u32 = 7;

/// Maximum keymap size accepted before refusing allocation.
pub const MAX_KEYMAP: u32 = 16 << 20;

/// What one seat's devices said, in order.
#[derive(Debug)]
pub enum Input {
    /// Seat capability bitmask (`wl_seat.capability`) changed.
    Capabilities {
        seat: u32,
        capabilities: u32,
    },
    /// One pointer frame.
    Pointer {
        seat: u32,
        frame: Vec<PointerEvent<Proxy>>,
    },
    Keyboard {
        seat: u32,
        event: KeyboardEvent,
    },
    Touch {
        seat: u32,
        event: TouchEvent,
    },
}

/// A `wl_keyboard` event.
#[derive(Debug)]
pub enum KeyboardEvent {
    /// The keymap, read from the descriptor the compositor sent.
    Keymap(Result<Keymap, KeymapError>),
    /// Keyboard entered `surface` (`None` if destroyed) with `keys` (evdev codes) held.
    Enter {
        serial: u32,
        surface: Option<Proxy>,
        keys: Vec<u32>,
    },
    Leave {
        serial: u32,
        surface: Option<Proxy>,
    },
    /// Key press or release (`key` is evdev code; `state` is `wl_keyboard.key_state`).
    Key {
        serial: u32,
        time: u32,
        key: u32,
        state: u32,
    },
    Modifiers {
        serial: u32,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    },
    /// Characters per second (0: no repeat) and the delay in milliseconds.
    RepeatInfo {
        rate: i32,
        delay: i32,
    },
}

/// A keymap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    /// `wl_keyboard.keymap_format`.
    pub format: u32,
    /// The keymap's text up to its terminating NUL (empty for `no_keymap`).
    pub text: Vec<u8>,
}

/// Why a keymap could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapError {
    /// The size is past [`MAX_KEYMAP`].
    TooLarge(u32),
    /// The descriptor could not be read for the size it came with.
    Read(std::io::ErrorKind),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::TooLarge(n) => write!(f, "a keymap of {n} bytes is past {MAX_KEYMAP}"),
            KeymapError::Read(kind) => write!(f, "the keymap could not be read: {kind}"),
        }
    }
}

impl std::error::Error for KeymapError {}

/// Read a keymap of `size` bytes from `fd` at offset 0 without mapping it.
pub fn read_keymap(format: u32, fd: OwnedFd, size: u32) -> Result<Keymap, KeymapError> {
    if format != wl_keyboard::keymap_format::XKB_V1 {
        return Ok(Keymap {
            format,
            text: Vec::new(),
        });
    }
    if size > MAX_KEYMAP {
        return Err(KeymapError::TooLarge(size));
    }
    let file = File::from(fd);
    let len = usize::try_from(size).map_err(|_| KeymapError::TooLarge(size))?;
    let mut text = vec![0; len];
    file.read_exact_at(&mut text, 0)
        .map_err(|e| KeymapError::Read(e.kind()))?;
    if let Some(nul) = text.iter().position(|&b| b == 0) {
        text.truncate(nul);
    }
    Ok(Keymap { format, text })
}

/// A `wl_touch` event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchEvent {
    Down {
        serial: u32,
        time: u32,
        surface: Option<Proxy>,
        id: i32,
        x: f64,
        y: f64,
    },
    Up {
        serial: u32,
        time: u32,
        id: i32,
    },
    Motion {
        time: u32,
        id: i32,
        x: f64,
        y: f64,
    },
    Frame,
    Cancel,
    Shape {
        id: i32,
        major: f64,
        minor: f64,
    },
    Orientation {
        id: i32,
        orientation: f64,
    },
}

/// Live seat proxies and pointer serials lent by [`Seats::with_seat`].
#[derive(Clone, Copy, Debug)]
pub struct SeatRefs {
    /// The `wl_seat`.
    pub seat: Proxy,
    /// Its `wl_pointer`, while it has one.
    pub pointer: Option<Proxy>,
    /// The serial of the pointer's newest `enter` (what `set_cursor` takes).
    pub pointer_enter_serial: Option<u32>,
    /// Serial of the pointer's newest button event (for move, resize, or window menu).
    pub pointer_button_serial: Option<u32>,
}

/// A seat as it describes itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatInfo {
    /// The registry's name for the global: how this module names the seat.
    pub id: u32,
    /// `wl_seat.name` (version 2), such as `seat0`.
    pub name: Option<String>,
    pub capabilities: u32,
}

/// A device this module made. Dropping it releases it (a destructor request
/// from version 3, the proxy alone before), then frees its listener, so a
/// listener never outlives its proxy's events.
struct Device {
    proxy: Proxy,
    version: u32,
    /// `wl_pointer::release`, `wl_keyboard::release` or `wl_touch::release`.
    release: unsafe fn(Proxy),
    listener: Option<Listener>,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: a device this module made, released once; its listener is freed after.
        unsafe {
            if self.version >= 3 {
                (self.release)(self.proxy);
            } else {
                self.proxy.destroy();
            }
        }
        drop(self.listener.take());
    }
}

/// The devices a seat holds, by capability (released as they drop).
#[derive(Default)]
struct Devices {
    pointer: Option<Device>,
    keyboard: Option<Device>,
    touch: Option<Device>,
}

impl Devices {
    /// Take each of `made`'s devices whose place is empty; the rest come
    /// back.
    fn adopt(&mut self, made: Devices) -> Devices {
        let mut spare = Devices::default();
        for (place, new, left) in [
            (&mut self.pointer, made.pointer, &mut spare.pointer),
            (&mut self.keyboard, made.keyboard, &mut spare.keyboard),
            (&mut self.touch, made.touch, &mut spare.touch),
        ] {
            match place {
                Some(_) => *left = new,
                None => *place = new,
            }
        }
        spare
    }
}

struct SeatSlot {
    id: u32,
    proxy: Proxy,
    version: u32,
    name: Option<String>,
    capabilities: u32,
    devices: Devices,
    frames: Frames<Proxy>,
    keyboard_serial: Option<u32>,
    keyboard_focus: Option<Proxy>,
    listener: Option<Listener>,
}

/// What the seats' listeners and the caller share. No call that the lock
/// census resolves by name (a free or qualified function, `self.name(..)`)
/// is made while it is held, and no function with a common name takes it, so
/// it is a leaf of the process's lock graph. The caller's sink is called
/// with it unlocked.
#[derive(Default)]
struct SeatBook {
    seats: Vec<SeatSlot>,
}

impl SeatBook {
    fn slot(&mut self, id: u32) -> Option<&mut SeatSlot> {
        self.seats.iter_mut().find(|s| s.id == id)
    }
}

/// Every seat of a registry, bound and listened to.
pub struct Seats {
    seat_book: Arc<Mutex<SeatBook>>,
    /// Where every seat's input goes.
    sink: Sink<Input>,
}

fn text(s: Option<&CStr>) -> Option<String> {
    s.map(|s| s.to_string_lossy().into_owned())
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_ne_bytes(*w))
        .collect()
}

impl Seats {
    /// Bind every `wl_seat` in `registry` and forward decoded input to `sink`.
    pub fn bind_all(registry: &Registry, sink: Sink<Input>) -> Result<Seats, Error> {
        let seats = Seats {
            seat_book: Arc::new(Mutex::new(SeatBook::default())),
            sink,
        };
        seats.sync(registry)?;
        Ok(seats)
    }

    /// Bind newly advertised seats in `registry` and release removed ones.
    pub fn sync(&self, registry: &Registry) -> Result<(), Error> {
        let globals: Vec<_> = registry
            .globals()
            .into_iter()
            .filter(|g| g.interface.as_c_str() == c"wl_seat")
            .collect();
        let (gone, bound): (Vec<SeatSlot>, Vec<u32>) = {
            let mut seat_book = self
                .seat_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let (keep, gone): (Vec<SeatSlot>, Vec<SeatSlot>) = seat_book
                .seats
                .drain(..)
                .partition(|s| globals.iter().any(|g| g.name == s.id));
            seat_book.seats = keep;
            let bound = seat_book.seats.iter().map(|s| s.id).collect();
            (gone, bound)
        };
        drop(gone);
        for global in globals.iter().filter(|g| !bound.contains(&g.name)) {
            let version = global.version.min(VERSION);
            let proxy = registry
                .bind(global, &wl_seat::INTERFACE, version)
                .ok_or(Error::NoObject)?;
            let id = global.name;
            // SAFETY: a seat this module bound, without a listener; the
            // listener is freed after the seat is released (SeatSlot's drop).
            let listener = match unsafe { seat_listener(proxy, &self.seat_book, id, &self.sink) } {
                Ok(l) => l,
                Err(e) => {
                    // SAFETY: the seat bound above, not used again.
                    unsafe { proxy.destroy() };
                    return Err(e);
                }
            };
            let slot = SeatSlot {
                id,
                proxy,
                version,
                name: None,
                capabilities: 0,
                devices: Devices::default(),
                frames: Frames::new(0),
                keyboard_serial: None,
                keyboard_focus: None,
                listener: Some(listener),
            };
            self.seat_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .seats
                .push(slot);
        }
        Ok(())
    }

    /// Every seat, as of the last dispatch.
    pub fn infos(&self) -> Vec<SeatInfo> {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seats
            .iter()
            .map(|s| SeatInfo {
                id: s.id,
                name: s.name.clone(),
                capabilities: s.capabilities,
            })
            .collect()
    }

    /// Return the seat's `wl_seat` proxy.
    pub fn proxy(&self, seat: u32) -> Option<Proxy> {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot(seat)
            .map(|s| s.proxy)
    }

    /// The seat's `wl_pointer`, while it has one.
    pub fn pointer(&self, seat: u32) -> Option<Proxy> {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot(seat)
            .and_then(|s| s.devices.pointer.as_ref().map(|d| d.proxy))
    }

    /// The serial of the pointer's newest `enter` (what `set_cursor` takes).
    pub fn pointer_enter_serial(&self, seat: u32) -> Option<u32> {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot(seat)
            .and_then(|s| s.frames.latest_enter())
    }

    /// Serial of the pointer's newest button event (for move, resize, or drag).
    pub fn pointer_button_serial(&self, seat: u32) -> Option<u32> {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot(seat)
            .and_then(|s| s.frames.latest_button())
    }

    /// Newest serial the seat's keyboard (enter, key, modifiers) or pointer (enter, button) gave.
    pub fn latest_serial(&self, seat: u32) -> Option<u32> {
        let mut seat_book = self
            .seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let s = seat_book.slot(seat)?;
        [
            s.keyboard_serial,
            s.frames.latest_enter(),
            s.frames.latest_button(),
        ]
        .into_iter()
        .flatten()
        .max()
    }

    /// Surface currently holding the seat's keyboard focus, if any.
    pub fn keyboard_focus(&self, seat: u32) -> Option<Proxy> {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot(seat)
            .and_then(|s| s.keyboard_focus)
    }

    /// Run `request` with the seat's live objects and serials while holding the seat book lock.
    pub fn with_seat<R>(&self, seat: u32, request: impl FnOnce(&SeatRefs) -> R) -> Option<R> {
        let mut seat_book = self
            .seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let s = seat_book.slot(seat)?;
        let refs = SeatRefs {
            seat: s.proxy,
            pointer: s.devices.pointer.as_ref().map(|d| d.proxy),
            pointer_enter_serial: s.frames.latest_enter(),
            pointer_button_serial: s.frames.latest_button(),
        };
        Some(request(&refs))
    }

    /// Whether the seat has a keyboard bound.
    pub fn has_keyboard(&self, seat: u32) -> bool {
        self.seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot(seat)
            .is_some_and(|s| s.devices.keyboard.is_some())
    }
}

impl Drop for Seats {
    fn drop(&mut self) {
        let seats: Vec<SeatSlot> = self
            .seat_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seats
            .split_off(0);
        drop(seats);
    }
}

impl Drop for SeatSlot {
    /// Release the seat's devices, then the seat, then its listener.
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.devices));
        // SAFETY: a seat this module bound, released once.
        unsafe {
            if self.version >= 5 {
                wl_seat::release(self.proxy);
            } else {
                self.proxy.destroy();
            }
        }
        drop(self.listener.take());
    }
}

/// Listen to a seat.
unsafe fn seat_listener(
    proxy: Proxy,
    seat_book: &Arc<Mutex<SeatBook>>,
    id: u32,
    sink: &Sink<Input>,
) -> Result<Listener, Error> {
    let weak = Arc::downgrade(seat_book);
    let sink = Arc::clone(sink);
    // SAFETY: the caller's contract.
    unsafe {
        proxy.listen(move |seat, opcode, args| {
            if let Some(seat_book) = weak.upgrade() {
                on_seat(&seat_book, &weak, &sink, id, seat, opcode, args);
            }
        })
    }
}

/// A `wl_seat` event: its name, or its capabilities, whose devices are made
/// and released here with the book unlocked (making one takes no lock, and a
/// release frees a listener), then handed to `sink`.
fn on_seat(
    seat_book: &Mutex<SeatBook>,
    weak: &Weak<Mutex<SeatBook>>,
    sink: &Sink<Input>,
    id: u32,
    seat: Proxy,
    opcode: u32,
    args: &[EventArg<'_>],
) {
    let caps = match (opcode, args) {
        (wl_seat::evt::NAME, [EventArg::Str(name)]) => {
            let name = text(*name);
            if let Some(slot) = seat_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .slot(id)
            {
                slot.name = name;
            }
            return;
        }
        (wl_seat::evt::CAPABILITIES, [EventArg::Uint(caps)]) => *caps,
        _ => return,
    };
    let (pointer, keyboard, touch) = (
        caps & wl_seat::capability::POINTER != 0,
        caps & wl_seat::capability::KEYBOARD != 0,
        caps & wl_seat::capability::TOUCH != 0,
    );
    let (gone, want) = {
        let mut seat_book = seat_book.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(slot) = seat_book.slot(id) else {
            return;
        };
        slot.capabilities = caps;
        let d = &mut slot.devices;
        let gone = Devices {
            pointer: d.pointer.take_if(|_| !pointer),
            keyboard: d.keyboard.take_if(|_| !keyboard),
            touch: d.touch.take_if(|_| !touch),
        };
        if gone.keyboard.is_some() {
            slot.keyboard_focus = None;
        }
        let want = (
            pointer && d.pointer.is_none(),
            keyboard && d.keyboard.is_none(),
            touch && d.touch.is_none(),
        );
        (gone, want)
    };
    let pointer_gone = gone.pointer.is_some();
    drop(gone);
    // SAFETY: the live seat the event came from, which has each capability asked for.
    let made = unsafe {
        let keyboard = want
            .1
            .then(|| wl_seat::get_keyboard(seat))
            .flatten()
            .map(|k| device(k, weak, sink, id, keyboard_input, wl_keyboard::release));
        let pointer = want
            .0
            .then(|| wl_seat::get_pointer(seat))
            .flatten()
            .map(|p| device(p, weak, sink, id, pointer_input, wl_pointer::release));
        let touch = want
            .2
            .then(|| wl_seat::get_touch(seat))
            .flatten()
            .map(|t| device(t, weak, sink, id, touch_input, wl_touch::release));
        Devices {
            pointer,
            keyboard,
            touch,
        }
    };
    let frames = (pointer_gone || made.pointer.is_some())
        .then(|| Frames::new(made.pointer.as_ref().map_or(0, |d| d.version)));
    let (orphans, live): (Devices, bool) = {
        let mut seat_book = seat_book.lock().unwrap_or_else(PoisonError::into_inner);
        match seat_book.slot(id) {
            Some(slot) => {
                let spare = slot.devices.adopt(made);
                if let Some(frames) = frames {
                    slot.frames = frames;
                }
                (spare, true)
            }
            None => (made, false),
        }
    };
    drop(orphans);
    if live {
        sink(Input::Capabilities {
            seat: id,
            capabilities: caps,
        });
    }
}

/// A device's event, decoded: what the listener records.
enum DeviceInput {
    Pointer(Raw<Proxy>),
    Keyboard(KeyboardEvent),
    Touch(TouchEvent),
}

type Decode = fn(u32, &[EventArg<'_>]) -> Option<DeviceInput>;

/// A device the seat made, its events decoded by `decode` (with the book
/// unlocked: a keymap is read there), recorded, and handed to `sink` with
/// the book unlocked again.
fn device(
    proxy: Proxy,
    weak: &Weak<Mutex<SeatBook>>,
    sink: &Sink<Input>,
    id: u32,
    decode: Decode,
    release: unsafe fn(Proxy),
) -> Device {
    let weak = weak.clone();
    let sink = Arc::clone(sink);
    // SAFETY: a device made from the seat just now, without a listener;
    // it is released before its listener is freed (Device's drop).
    let listener = unsafe {
        proxy.listen(move |_, opcode, args| {
            let Some(input) = decode(opcode, args) else {
                return;
            };
            if let Some(heard) = weak.upgrade().and_then(|seat_book| {
                seat_book
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record(id, input)
            }) {
                sink(heard);
            }
        })
    };
    let listener = match listener {
        Ok(l) => Some(l),
        Err(e) => {
            aterm_log::warn!("wayland: a seat device has no listener: {e}");
            None
        }
    };
    Device {
        // SAFETY: the live device.
        version: unsafe { proxy.version() },
        proxy,
        release,
        listener,
    }
}

impl SeatBook {
    /// Record one device event of seat `id`: a pointer's into its frame (a
    /// completed frame is input), a keyboard's serial and focus. The input
    /// it makes, for the sink; `None` while a pointer frame is open, or for
    /// a seat released meanwhile.
    fn record(&mut self, id: u32, input: DeviceInput) -> Option<Input> {
        let slot = self.seats.iter_mut().find(|s| s.id == id)?;
        Some(match input {
            DeviceInput::Pointer(raw) => Input::Pointer {
                seat: id,
                frame: slot.frames.feed(raw)?,
            },
            DeviceInput::Keyboard(event) => {
                match &event {
                    KeyboardEvent::Enter {
                        serial, surface, ..
                    } => {
                        slot.keyboard_serial = Some(*serial);
                        slot.keyboard_focus = *surface;
                    }
                    KeyboardEvent::Leave { .. } => slot.keyboard_focus = None,
                    KeyboardEvent::Key { serial, .. } | KeyboardEvent::Modifiers { serial, .. } => {
                        slot.keyboard_serial = Some(*serial);
                    }
                    _ => {}
                }
                Input::Keyboard { seat: id, event }
            }
            DeviceInput::Touch(event) => Input::Touch { seat: id, event },
        })
    }
}

fn fixed(f: Fixed) -> f64 {
    f.to_f64()
}

fn pointer_input(opcode: u32, args: &[EventArg<'_>]) -> Option<DeviceInput> {
    use wl_pointer::evt;
    let raw = match (opcode, args) {
        (
            evt::ENTER,
            [
                EventArg::Uint(serial),
                EventArg::Object(surface),
                EventArg::Fixed(x),
                EventArg::Fixed(y),
            ],
        ) => Raw::Enter {
            serial: *serial,
            surface: *surface,
            x: fixed(*x),
            y: fixed(*y),
        },
        (evt::LEAVE, [EventArg::Uint(serial), EventArg::Object(surface)]) => Raw::Leave {
            serial: *serial,
            surface: *surface,
        },
        (evt::MOTION, [EventArg::Uint(time), EventArg::Fixed(x), EventArg::Fixed(y)]) => {
            Raw::Motion {
                time: *time,
                x: fixed(*x),
                y: fixed(*y),
            }
        }
        (
            evt::BUTTON,
            [
                EventArg::Uint(serial),
                EventArg::Uint(time),
                EventArg::Uint(button),
                EventArg::Uint(state),
            ],
        ) => Raw::Button {
            serial: *serial,
            time: *time,
            button: *button,
            state: *state,
        },
        (
            evt::AXIS,
            [
                EventArg::Uint(time),
                EventArg::Uint(axis),
                EventArg::Fixed(v),
            ],
        ) => Raw::Axis {
            time: *time,
            axis: *axis,
            value: fixed(*v),
        },
        (evt::FRAME, []) => Raw::Frame,
        (evt::AXIS_SOURCE, [EventArg::Uint(source)]) => Raw::AxisSource(*source),
        (evt::AXIS_STOP, [EventArg::Uint(time), EventArg::Uint(axis)]) => Raw::AxisStop {
            time: *time,
            axis: *axis,
        },
        (evt::AXIS_DISCRETE, [EventArg::Uint(axis), EventArg::Int(discrete)]) => {
            Raw::AxisDiscrete {
                axis: *axis,
                discrete: *discrete,
            }
        }
        _ => return None,
    };
    Some(DeviceInput::Pointer(raw))
}

fn keyboard_input(opcode: u32, args: &[EventArg<'_>]) -> Option<DeviceInput> {
    use wl_keyboard::evt;
    let event = match (opcode, args) {
        (
            evt::KEYMAP,
            [
                EventArg::Uint(format),
                EventArg::Fd(fd),
                EventArg::Uint(size),
            ],
        ) => {
            // SAFETY: the event's descriptor, which the receiver owns.
            let fd = unsafe { OwnedFd::from_raw_fd(*fd) };
            KeyboardEvent::Keymap(read_keymap(*format, fd, *size))
        }
        (
            evt::ENTER,
            [
                EventArg::Uint(serial),
                EventArg::Object(surface),
                EventArg::Array(keys),
            ],
        ) => KeyboardEvent::Enter {
            serial: *serial,
            surface: *surface,
            keys: words(keys),
        },
        (evt::LEAVE, [EventArg::Uint(serial), EventArg::Object(surface)]) => KeyboardEvent::Leave {
            serial: *serial,
            surface: *surface,
        },
        (
            evt::KEY,
            [
                EventArg::Uint(serial),
                EventArg::Uint(time),
                EventArg::Uint(key),
                EventArg::Uint(state),
            ],
        ) => KeyboardEvent::Key {
            serial: *serial,
            time: *time,
            key: *key,
            state: *state,
        },
        (
            evt::MODIFIERS,
            [
                EventArg::Uint(serial),
                EventArg::Uint(depressed),
                EventArg::Uint(latched),
                EventArg::Uint(locked),
                EventArg::Uint(group),
            ],
        ) => KeyboardEvent::Modifiers {
            serial: *serial,
            depressed: *depressed,
            latched: *latched,
            locked: *locked,
            group: *group,
        },
        (evt::REPEAT_INFO, [EventArg::Int(rate), EventArg::Int(delay)]) => {
            KeyboardEvent::RepeatInfo {
                rate: *rate,
                delay: *delay,
            }
        }
        _ => return None,
    };
    Some(DeviceInput::Keyboard(event))
}

fn touch_input(opcode: u32, args: &[EventArg<'_>]) -> Option<DeviceInput> {
    use wl_touch::evt;
    let event = match (opcode, args) {
        (
            evt::DOWN,
            [
                EventArg::Uint(serial),
                EventArg::Uint(time),
                EventArg::Object(surface),
                EventArg::Int(touch),
                EventArg::Fixed(x),
                EventArg::Fixed(y),
            ],
        ) => TouchEvent::Down {
            serial: *serial,
            time: *time,
            surface: *surface,
            id: *touch,
            x: fixed(*x),
            y: fixed(*y),
        },
        (
            evt::UP,
            [
                EventArg::Uint(serial),
                EventArg::Uint(time),
                EventArg::Int(touch),
            ],
        ) => TouchEvent::Up {
            serial: *serial,
            time: *time,
            id: *touch,
        },
        (
            evt::MOTION,
            [
                EventArg::Uint(time),
                EventArg::Int(touch),
                EventArg::Fixed(x),
                EventArg::Fixed(y),
            ],
        ) => TouchEvent::Motion {
            time: *time,
            id: *touch,
            x: fixed(*x),
            y: fixed(*y),
        },
        (evt::FRAME, []) => TouchEvent::Frame,
        (evt::CANCEL, []) => TouchEvent::Cancel,
        (
            evt::SHAPE,
            [
                EventArg::Int(touch),
                EventArg::Fixed(major),
                EventArg::Fixed(minor),
            ],
        ) => TouchEvent::Shape {
            id: *touch,
            major: fixed(*major),
            minor: fixed(*minor),
        },
        (evt::ORIENTATION, [EventArg::Int(touch), EventArg::Fixed(o)]) => TouchEvent::Orientation {
            id: *touch,
            orientation: fixed(*o),
        },
        _ => return None,
    };
    Some(DeviceInput::Touch(event))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn file_with(bytes: &[u8]) -> (aterm_tempfile::TempDir, OwnedFd) {
        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let path = dir.path().join("keymap");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(bytes))
            .expect("the keymap file");
        let fd = OwnedFd::from(File::open(&path).expect("open"));
        (dir, fd)
    }

    #[test]
    fn a_keymap_is_read_to_its_nul() {
        let body = b"xkb_keymap { };\n\0";
        let (_dir, fd) = file_with(body);
        let size = u32::try_from(body.len()).unwrap();
        let keymap = read_keymap(wl_keyboard::keymap_format::XKB_V1, fd, size).unwrap();
        assert_eq!(keymap.text, b"xkb_keymap { };\n");
    }

    #[test]
    fn a_keymap_shorter_than_its_size_is_an_error_not_a_guess() {
        let (_dir, fd) = file_with(b"xkb_keymap");
        assert_eq!(
            read_keymap(wl_keyboard::keymap_format::XKB_V1, fd, 64),
            Err(KeymapError::Read(std::io::ErrorKind::UnexpectedEof))
        );
    }

    #[test]
    fn a_keymap_past_the_limit_is_refused_unread() {
        let (_dir, fd) = file_with(b"x");
        assert_eq!(
            read_keymap(wl_keyboard::keymap_format::XKB_V1, fd, MAX_KEYMAP + 1),
            Err(KeymapError::TooLarge(MAX_KEYMAP + 1))
        );
    }

    #[test]
    fn no_keymap_reads_nothing() {
        let (_dir, fd) = file_with(b"ignored");
        let keymap = read_keymap(wl_keyboard::keymap_format::NO_KEYMAP, fd, 7).unwrap();
        assert!(keymap.text.is_empty());
    }

    #[test]
    fn enter_keys_are_native_words() {
        let mut bytes = Vec::new();
        for k in [30u32, 48] {
            bytes.extend(k.to_ne_bytes());
        }
        assert_eq!(words(&bytes), [30, 48]);
    }
}
