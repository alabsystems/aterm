// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selections and drag and drop: a seat's `wl_data_device` (the clipboard, and drags) and `zwp_primary_selection_device_v1` (the primary selection), the offers the compositor hands them, the sources this client offers, and the pipes the bytes cross.

use std::ffi::{CStr, CString};
use std::io::{self, PipeReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::globals::Registry;
use crate::protocols::wayland::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
};
use crate::protocols::wp_primary_selection_unstable_v1::{
    zwp_primary_selection_device_manager_v1 as primary_manager,
    zwp_primary_selection_device_v1 as primary_device,
    zwp_primary_selection_offer_v1 as primary_offer,
    zwp_primary_selection_source_v1 as primary_source,
};
use crate::proxy::{Error, EventArg, Listener, Proxy};
use crate::sink::Sink;

/// The highest `wl_data_device_manager` version bound (smithay-client-toolkit 0.19.2's, the winit fork's data device until LW.7).
pub const DATA_DEVICE_VERSION: u32 = 3;

/// The most MIME types one offer keeps; a compositor that announces more is heard no further.
pub const MAX_MIME_TYPES: usize = 256;

/// Which selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `wl_data_device`: the clipboard, and drags.
    Clipboard,
    /// `zwp_primary_selection_device_v1`: the primary selection.
    Primary,
}

/// A device manager: `wl_data_device_manager` or `zwp_primary_selection_device_manager_v1`.
pub struct Manager {
    kind: Kind,
    proxy: Proxy,
    version: u32,
}

impl Manager {
    /// Bind `kind`'s manager, if the registry lists it.
    pub fn bind(registry: &Registry, kind: Kind) -> Option<Manager> {
        let (name, iface, max) = match kind {
            Kind::Clipboard => (
                c"wl_data_device_manager",
                &wl_data_device_manager::INTERFACE,
                DATA_DEVICE_VERSION,
            ),
            Kind::Primary => (
                c"zwp_primary_selection_device_manager_v1",
                &primary_manager::INTERFACE,
                1,
            ),
        };
        let global = registry.find(name)?;
        let version = global.version.min(max).max(1);
        let proxy = registry.bind(&global, iface, version)?;
        Some(Manager {
            kind,
            proxy,
            version,
        })
    }

    /// Which selection it manages.
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// The version bound.
    pub const fn version(&self) -> u32 {
        self.version
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // SAFETY: the manager this value bound, destroyed once; its devices and sources are its caller's to end first. wl_data_device_manager has no destructor request before the core protocol's version 4, which is never bound.
        unsafe {
            match self.kind {
                Kind::Clipboard => self.proxy.destroy(),
                Kind::Primary => primary_manager::destroy(self.proxy),
            }
        }
    }
}

/// What an offer says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OfferInfo {
    pub mime_types: Vec<String>,
    /// A drag source's `wl_data_device_manager.dnd_action` bits (version 3).
    pub source_actions: u32,
    /// The action the compositor chose for a drag (version 3).
    pub action: u32,
}

/// A drag-and-drop event of a clipboard device.
#[derive(Clone, Debug, PartialEq)]
pub enum DragEvent {
    /// A drag entered `surface`; `offer` is what it carries (`None` for a drag within one client that offers nothing).
    Enter {
        serial: u32,
        surface: Option<Proxy>,
        x: f64,
        y: f64,
        offer: Option<OfferInfo>,
    },
    Motion {
        time: u32,
        x: f64,
        y: f64,
    },
    Leave,
    /// Dropped: read the offer, then [finish](Device::finish_drag) it.
    Drop,
}

/// An offer this device received. Dropping it destroys it, then frees its
/// listener; the device drops its offers with the book unlocked.
struct OfferSlot {
    kind: Kind,
    proxy: Proxy,
    offer_book: Arc<Mutex<OfferInfo>>,
    listener: Option<Listener>,
}

impl OfferSlot {
    /// What the offer has said so far.
    fn snapshot(&self) -> OfferInfo {
        self.offer_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for OfferSlot {
    fn drop(&mut self) {
        // SAFETY: an offer this device received, destroyed once; its listener is freed after.
        unsafe {
            match self.kind {
                Kind::Clipboard => wl_data_offer::destroy(self.proxy),
                Kind::Primary => primary_offer::destroy(self.proxy),
            }
        }
        drop(self.listener.take());
    }
}

struct Drag {
    offer: Option<Proxy>,
    serial: u32,
    left: bool,
    dropped: bool,
}

/// What a device's listener and the caller share. As `seat::SeatBook`, it is
/// a leaf of the lock graph: nothing the lock census resolves by name is
/// called while it is held, and offers are made and destroyed, and the
/// caller's sink called, with it unlocked.
struct DeviceBook {
    /// Every live offer, in arrival order.
    offers: Vec<OfferSlot>,
    selection: Option<Proxy>,
    drag: Option<Drag>,
}

impl DeviceBook {
    fn info(&self, offer: Proxy) -> Option<OfferInfo> {
        self.offers
            .iter()
            .find(|o| o.proxy == offer)
            .map(OfferSlot::snapshot)
    }

    /// Take `offer` out of the book, to be ended.
    fn remove(&mut self, offer: Proxy) -> Option<OfferSlot> {
        let i = self.offers.iter().position(|o| o.proxy == offer)?;
        Some(self.offers.remove(i))
    }

    /// Take out `old` unless the selection or the drag still holds it.
    fn remove_unheld(&mut self, old: Option<Proxy>) -> Option<OfferSlot> {
        let old = old?;
        let held =
            self.selection == Some(old) || self.drag.as_ref().and_then(|d| d.offer) == Some(old);
        if held { None } else { self.remove(old) }
    }

    /// Take out the offers announced before `used` that are neither the
    /// selection nor the drag: announced, never used, and now stale.
    fn remove_stale_before(&mut self, used: Proxy) -> Vec<OfferSlot> {
        let Some(at) = self.offers.iter().position(|o| o.proxy == used) else {
            return Vec::new();
        };
        let keep = [
            Some(used),
            self.selection,
            self.drag.as_ref().and_then(|d| d.offer),
        ];
        let stale: Vec<Proxy> = self.offers[..at]
            .iter()
            .map(|o| o.proxy)
            .filter(|p| !keep.contains(&Some(*p)))
            .collect();
        stale.into_iter().filter_map(|p| self.remove(p)).collect()
    }
}

/// A seat's data device or primary-selection device.
pub struct Device {
    kind: Kind,
    proxy: Proxy,
    version: u32,
    device_book: Arc<Mutex<DeviceBook>>,
    listener: Option<Listener>,
}

impl Device {
    /// The device of `manager`'s kind on `seat`.
    /// # Safety
    /// `seat` is a live `wl_seat` on the manager's display and outlives the device.
    pub unsafe fn new(
        manager: &Manager,
        seat: Proxy,
        sink: Sink<DragEvent>,
    ) -> Result<Device, Error> {
        // SAFETY: the manager is live (owned by its value); the caller's seat.
        let proxy = unsafe {
            match manager.kind {
                Kind::Clipboard => wl_data_device_manager::get_data_device(manager.proxy, seat),
                Kind::Primary => primary_manager::get_device(manager.proxy, seat),
            }
        }
        .ok_or(Error::NoObject)?;
        let device_book = Arc::new(Mutex::new(DeviceBook {
            offers: Vec::new(),
            selection: None,
            drag: None,
        }));
        // SAFETY: a device made here, without a listener; the listener is freed after the device is released (Drop).
        let listener = match unsafe { device_listener(proxy, &device_book, manager.kind, sink) } {
            Ok(l) => l,
            Err(e) => {
                // SAFETY: the device made above, not used again.
                unsafe { proxy.destroy() };
                return Err(e);
            }
        };
        Ok(Device {
            kind: manager.kind,
            proxy,
            // SAFETY: the live device.
            version: unsafe { proxy.version() },
            device_book,
            listener: Some(listener),
        })
    }

    /// Which selection it carries.
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// The current selection's offer, as of the last dispatch; `None` when nothing is selected (or this client's keyboard is elsewhere: a compositor sends the selection to the focused client).
    pub fn selection(&self) -> Option<OfferInfo> {
        let device_book = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        device_book.selection.and_then(|o| device_book.info(o))
    }

    /// Ask the selection's source for its bytes as `mime_type`: the read end of the pipe they arrive on.
    pub fn receive_selection(&self, mime_type: &CStr) -> io::Result<PipeReader> {
        let offer = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .selection;
        let offer = offer.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no selection"))?;
        receive(self.kind, offer, mime_type)
    }

    /// Make `source` the selection (`None` clears it), with the serial of the input that asked for it.
    pub fn set_selection(&self, source: Option<&Source>, serial: u32) -> bool {
        if source.is_some_and(|s| s.kind != self.kind) {
            return false;
        }
        let source = source.map(|s| s.proxy);
        // SAFETY: the live device; a live source of its kind.
        unsafe {
            match self.kind {
                Kind::Clipboard => wl_data_device::set_selection(self.proxy, source, serial),
                Kind::Primary => primary_device::set_selection(self.proxy, source, serial),
            }
        }
        true
    }

    /// The drag's offer, as of the last dispatch.
    pub fn drag_offer(&self) -> Option<OfferInfo> {
        let device_book = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        device_book
            .drag
            .as_ref()
            .and_then(|d| d.offer)
            .and_then(|o| device_book.info(o))
    }

    /// Answer the drag over this client: `mime_type` is what a drop would be read as (`None`: no drop here), `actions` the actions this client takes and `preferred` the one it prefers (version 3).
    pub fn accept_drag(&self, mime_type: Option<&CStr>, actions: u32, preferred: u32) -> bool {
        let over = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drag
            .as_ref()
            .filter(|d| !d.left)
            .and_then(|d| d.offer.map(|o| (o, d.serial)));
        let Some((offer, serial)) = over else {
            return false;
        };
        // SAFETY: the live drag offer.
        unsafe {
            wl_data_offer::accept(offer, serial, mime_type);
            if self.version >= 3 {
                wl_data_offer::set_actions(offer, actions, preferred);
            }
        }
        true
    }

    /// Refuse the drag over this client: no type would be read (`wl_data_offer.accept` with none), and no actions are named.
    pub fn reject_drag(&self) -> bool {
        let over = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drag
            .as_ref()
            .filter(|d| !d.left)
            .and_then(|d| d.offer.map(|o| (o, d.serial)));
        let Some((offer, serial)) = over else {
            return false;
        };
        // SAFETY: the live drag offer.
        unsafe { wl_data_offer::accept(offer, serial, None) };
        true
    }

    /// Take over the offer that was dropped on this client: the device forgets it, and the caller reads it, then [finishes](Dropped::finish) it (or drops it, which destroys it unfinished: a drop that was refused must not be finished).
    pub fn take_dropped(&self) -> Option<Dropped> {
        let slot = {
            let mut device_book = self
                .device_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let offer = device_book
                .drag
                .as_ref()
                .filter(|d| d.dropped)
                .and_then(|d| d.offer)?;
            device_book.drag = None;
            device_book.remove(offer)
        }?;
        Some(Dropped {
            info: slot.snapshot(),
            slot: Some(slot),
            version: self.version,
        })
    }

    /// Read the drag's offer as `mime_type`: while it is over this client, or after it dropped here.
    pub fn receive_drag(&self, mime_type: &CStr) -> io::Result<PipeReader> {
        let offer = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drag
            .as_ref()
            .filter(|d| !d.left || d.dropped)
            .and_then(|d| d.offer);
        let offer = offer
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no drag over this client"))?;
        receive(self.kind, offer, mime_type)
    }

    /// The drop is read: tell the source (`finish`, version 3) and destroy the offer.
    pub fn finish_drag(&self) {
        let (drag, slot) = {
            let mut device_book = self
                .device_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let drag = device_book.drag.take();
            let slot = device_book.remove_unheld(drag.as_ref().and_then(|d| d.offer));
            (drag, slot)
        };
        if let Some(Drag {
            offer: Some(offer),
            dropped: true,
            ..
        }) = drag
            && self.version >= 3
        {
            // SAFETY: the live, dropped offer, destroyed only below.
            unsafe { wl_data_offer::finish(offer) };
        }
        drop(slot);
    }

    /// Start a drag from `origin` (a surface of this client the pointer pressed on, `serial` that press's) carrying `source`, with `icon` as the drag's image.
    /// # Safety
    /// `origin` and `icon` are live `wl_surface`s of this client's display.
    pub unsafe fn start_drag(
        &self,
        source: Option<&Source>,
        origin: Proxy,
        icon: Option<Proxy>,
        serial: u32,
    ) -> bool {
        if self.kind != Kind::Clipboard || source.is_some_and(|s| s.kind != Kind::Clipboard) {
            return false;
        }
        // SAFETY: the live device and source; the caller's surfaces.
        unsafe {
            wl_data_device::start_drag(self.proxy, source.map(|s| s.proxy), origin, icon, serial);
        }
        true
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let offers: Vec<OfferSlot> = self
            .device_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .offers
            .split_off(0);
        drop(offers);
        // SAFETY: the device this value made, released once (`release` from version 2, the proxy alone before); its listener is freed after.
        unsafe {
            match self.kind {
                Kind::Clipboard if self.version >= 2 => wl_data_device::release(self.proxy),
                Kind::Clipboard => self.proxy.destroy(),
                Kind::Primary => primary_device::destroy(self.proxy),
            }
        }
        drop(self.listener.take());
    }
}

/// An offer dropped on this client and taken over from its device ([`Device::take_dropped`]).
pub struct Dropped {
    info: OfferInfo,
    slot: Option<OfferSlot>,
    version: u32,
}

impl Dropped {
    /// What the offer said: its types, and the action the compositor chose.
    pub const fn info(&self) -> &OfferInfo {
        &self.info
    }

    /// Ask the drag's source for its bytes as `mime_type`: the read end of the pipe they arrive on.
    pub fn receive(&self, mime_type: &CStr) -> io::Result<PipeReader> {
        let slot = self
            .slot
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no offer"))?;
        receive(slot.kind, slot.proxy, mime_type)
    }

    /// The drop is read: tell the source (`finish`, version 3), then destroy the offer.
    pub fn finish(mut self) {
        if let Some(slot) = self.slot.take() {
            if self.version >= 3 {
                // SAFETY: the live, dropped offer, destroyed only by the slot's drop just after.
                unsafe { wl_data_offer::finish(slot.proxy) };
            }
            drop(slot);
        }
    }
}

/// `receive` on `offer`: a fresh pipe, its write end sent, its read end
/// returned.
fn receive(kind: Kind, offer: Proxy, mime_type: &CStr) -> io::Result<PipeReader> {
    let (reader, writer) = io::pipe()?;
    // SAFETY: the live offer; libwayland duplicates the descriptor for the send, and this end closes when `writer` drops.
    unsafe {
        match kind {
            Kind::Clipboard => wl_data_offer::receive(offer, mime_type, writer.as_raw_fd()),
            Kind::Primary => primary_offer::receive(offer, mime_type, writer.as_raw_fd()),
        }
    }
    drop(writer);
    Ok(reader)
}

/// Listen to a device.
/// # Safety
/// `proxy` is a live device of `kind` this module made, without a listener,
/// released before the listener is freed.
unsafe fn device_listener(
    proxy: Proxy,
    device_book: &Arc<Mutex<DeviceBook>>,
    kind: Kind,
    sink: Sink<DragEvent>,
) -> Result<Listener, Error> {
    let weak = Arc::downgrade(device_book);
    // SAFETY: the caller's contract.
    unsafe {
        proxy.listen(move |_, opcode, args| {
            if let Some(device_book) = weak.upgrade()
                && let Some(event) = on_device(&device_book, kind, opcode, args)
            {
                sink(event);
            }
        })
    }
}

/// Listen to an offer: what it says goes to its book.
/// # Safety
/// `proxy` is a live offer of `kind` the device received, without a
/// listener, destroyed before the listener is freed.
unsafe fn offer_listener(
    proxy: Proxy,
    offer_book: &Arc<Mutex<OfferInfo>>,
    kind: Kind,
) -> Option<Listener> {
    let offer_book = Arc::clone(offer_book);
    // SAFETY: the caller's contract.
    let listener = unsafe {
        proxy.listen(move |_, opcode, args| {
            let said = match (opcode, args) {
                (wl_data_offer::evt::OFFER, [EventArg::Str(Some(mime))]) => {
                    Said::Mime(mime.to_string_lossy().into_owned())
                }
                _ if kind == Kind::Primary => return,
                (wl_data_offer::evt::SOURCE_ACTIONS, [EventArg::Uint(a)]) => {
                    Said::SourceActions(*a)
                }
                (wl_data_offer::evt::ACTION, [EventArg::Uint(a)]) => Said::Action(*a),
                _ => return,
            };
            offer_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .hear(said);
        })
    };
    listener.ok()
}

/// One thing an offer said.
enum Said {
    Mime(String),
    SourceActions(u32),
    Action(u32),
}

impl OfferInfo {
    fn hear(&mut self, said: Said) {
        match said {
            Said::Mime(m) => {
                if self.mime_types.len() < MAX_MIME_TYPES {
                    self.mime_types.push(m);
                }
            }
            Said::SourceActions(a) => self.source_actions = a,
            Said::Action(a) => self.action = a,
        }
    }
}

/// A device's events.
fn on_device(
    device_book: &Mutex<DeviceBook>,
    kind: Kind,
    opcode: u32,
    args: &[EventArg<'_>],
) -> Option<DragEvent> {
    let selection_op = match kind {
        Kind::Clipboard => wl_data_device::evt::SELECTION,
        Kind::Primary => primary_device::evt::SELECTION,
    };
    let (ended, heard): (Vec<OfferSlot>, Option<DragEvent>) = match (opcode, args) {
        (wl_data_device::evt::DATA_OFFER, [EventArg::NewId(Some(offer))]) => {
            let offer_book = Arc::new(Mutex::new(OfferInfo::default()));
            // SAFETY: the offer the event made, without a listener; it is destroyed before its listener is freed (OfferSlot's drop).
            let listener = unsafe { offer_listener(*offer, &offer_book, kind) };
            let slot = OfferSlot {
                kind,
                proxy: *offer,
                offer_book,
                listener,
            };
            device_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .offers
                .push(slot);
            (Vec::new(), None)
        }
        (op, [EventArg::Object(offer)]) if op == selection_op => {
            let mut device_book = device_book.lock().unwrap_or_else(PoisonError::into_inner);
            let old = device_book.selection.take();
            device_book.selection = *offer;
            let mut ended: Vec<OfferSlot> = device_book.remove_unheld(old).into_iter().collect();
            if let Some(o) = *offer {
                ended.append(&mut device_book.remove_stale_before(o));
            }
            (ended, None)
        }
        _ if kind == Kind::Primary => (Vec::new(), None),
        (
            wl_data_device::evt::ENTER,
            [
                EventArg::Uint(serial),
                EventArg::Object(surface),
                EventArg::Fixed(x),
                EventArg::Fixed(y),
                EventArg::Object(offer),
            ],
        ) => {
            let (x, y) = (x.to_f64(), y.to_f64());
            let mut device_book = device_book.lock().unwrap_or_else(PoisonError::into_inner);
            let old = device_book.drag.take().and_then(|d| d.offer);
            device_book.drag = Some(Drag {
                offer: *offer,
                serial: *serial,
                left: false,
                dropped: false,
            });
            let mut ended: Vec<OfferSlot> = device_book.remove_unheld(old).into_iter().collect();
            if let Some(o) = *offer {
                ended.append(&mut device_book.remove_stale_before(o));
            }
            let info = offer.and_then(|o| device_book.info(o));
            let enter = DragEvent::Enter {
                serial: *serial,
                surface: *surface,
                x,
                y,
                offer: info,
            };
            (ended, Some(enter))
        }
        (
            wl_data_device::evt::MOTION,
            [EventArg::Uint(time), EventArg::Fixed(x), EventArg::Fixed(y)],
        ) => {
            let motion = DragEvent::Motion {
                time: *time,
                x: x.to_f64(),
                y: y.to_f64(),
            };
            (Vec::new(), Some(motion))
        }
        (wl_data_device::evt::LEAVE, []) => {
            let mut device_book = device_book.lock().unwrap_or_else(PoisonError::into_inner);
            let ended = match device_book.drag.take() {
                Some(mut d) if d.dropped => {
                    d.left = true;
                    device_book.drag = Some(d);
                    Vec::new()
                }
                Some(d) => device_book.remove_unheld(d.offer).into_iter().collect(),
                None => Vec::new(),
            };
            (ended, Some(DragEvent::Leave))
        }
        (wl_data_device::evt::DROP, []) => {
            let mut device_book = device_book.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(d) = device_book.drag.as_mut() {
                d.dropped = true;
            }
            (Vec::new(), Some(DragEvent::Drop))
        }
        _ => (Vec::new(), None),
    };
    drop(ended);
    heard
}

/// What the compositor asked of a source.
#[derive(Debug)]
pub enum SourceEvent {
    /// Write the bytes for `mime_type` to `fd`, then close it.
    Send { mime_type: String, fd: OwnedFd },
    /// Replaced, or the drag ended without a drop: the source is spent.
    Cancelled,
    /// A drag's target accepted this type (`None`: it accepts none).
    Target(Option<String>),
    /// The compositor chose this action for the drag (version 3).
    Action(u32),
    /// The user dropped (version 3); the target may still be reading.
    DropPerformed,
    /// The target finished with the drop (version 3).
    Finished,
}

/// A selection or drag source this client offers.
pub struct Source {
    kind: Kind,
    proxy: Proxy,
    /// Set by the listener when the compositor cancels the source.
    cancelled: Arc<AtomicBool>,
    listener: Option<Listener>,
}

impl Source {
    /// A source of `manager`'s kind offering `mime_types`; for a drag, `dnd_actions` (`wl_data_device_manager.dnd_action` bits, version 3).
    pub fn new(
        manager: &Manager,
        mime_types: &[&CStr],
        dnd_actions: u32,
        sink: Sink<SourceEvent>,
    ) -> Result<Source, Error> {
        // SAFETY: the live manager.
        let proxy = unsafe {
            match manager.kind {
                Kind::Clipboard => wl_data_device_manager::create_data_source(manager.proxy),
                Kind::Primary => primary_manager::create_source(manager.proxy),
            }
        }
        .ok_or(Error::NoObject)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let kind = manager.kind;
        // SAFETY: a source made here, without a listener; it is destroyed before its listener is freed (Drop).
        let listener = match unsafe { source_listener(proxy, &cancelled, kind, sink) } {
            Ok(l) => l,
            Err(e) => {
                // SAFETY: the source made above, not used again.
                unsafe { proxy.destroy() };
                return Err(e);
            }
        };
        // SAFETY: the live source; NUL-terminated types.
        unsafe {
            for mime in mime_types {
                match kind {
                    Kind::Clipboard => wl_data_source::offer(proxy, mime),
                    Kind::Primary => primary_source::offer(proxy, mime),
                }
            }
            if kind == Kind::Clipboard && manager.version >= 3 && dnd_actions != 0 {
                wl_data_source::set_actions(proxy, dnd_actions);
            }
        }
        Ok(Source {
            kind,
            proxy,
            cancelled,
            listener: Some(listener),
        })
    }

    /// Which selection it serves.
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// Whether the compositor cancelled it (another source replaced it), as of the last dispatch.
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: the source this value made, destroyed once; its listener is freed after.
        unsafe {
            match self.kind {
                Kind::Clipboard => wl_data_source::destroy(self.proxy),
                Kind::Primary => primary_source::destroy(self.proxy),
            }
        }
        drop(self.listener.take());
    }
}

/// Listen to a source: each request, decoded, goes to `sink` (a
/// cancellation is also kept, for [`Source::cancelled`]).
///
/// # Safety
/// `proxy` is a live source of `kind` this module made, without a listener,
/// destroyed before the listener is freed.
unsafe fn source_listener(
    proxy: Proxy,
    cancelled: &Arc<AtomicBool>,
    kind: Kind,
    sink: Sink<SourceEvent>,
) -> Result<Listener, Error> {
    let cancelled = Arc::clone(cancelled);
    // SAFETY: the caller's contract.
    unsafe {
        proxy.listen(move |_, opcode, args| {
            let Some(request) = source_request(kind, opcode, args) else {
                return;
            };
            if matches!(request, SourceEvent::Cancelled) {
                cancelled.store(true, Ordering::Release);
            }
            sink(request);
        })
    }
}

/// A source's event, decoded.
fn source_request(kind: Kind, opcode: u32, args: &[EventArg<'_>]) -> Option<SourceEvent> {
    let (send, cancelled) = match kind {
        Kind::Clipboard => (wl_data_source::evt::SEND, wl_data_source::evt::CANCELLED),
        Kind::Primary => (primary_source::evt::SEND, primary_source::evt::CANCELLED),
    };
    Some(match (opcode, args) {
        (op, [EventArg::Str(mime), EventArg::Fd(fd)]) if op == send => SourceEvent::Send {
            mime_type: mime
                .map(|m| m.to_string_lossy().into_owned())
                .unwrap_or_default(),
            // SAFETY: the event's descriptor, which the receiver owns.
            fd: unsafe { OwnedFd::from_raw_fd(*fd) },
        },
        (op, []) if op == cancelled => SourceEvent::Cancelled,
        _ if kind == Kind::Primary => return None,
        (wl_data_source::evt::TARGET, [EventArg::Str(mime)]) => {
            SourceEvent::Target(mime.map(|m| m.to_string_lossy().into_owned()))
        }
        (wl_data_source::evt::DND_DROP_PERFORMED, []) => SourceEvent::DropPerformed,
        (wl_data_source::evt::DND_FINISHED, []) => SourceEvent::Finished,
        (wl_data_source::evt::ACTION, [EventArg::Uint(a)]) => SourceEvent::Action(*a),
        _ => return None,
    })
}

/// Write `bytes` to `fd` and close it, on a thread of its own: the reader may be slow, or this very client, reading only once its dispatching thread is free.
pub fn write_detached(fd: OwnedFd, bytes: Vec<u8>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("wayland-selection-write".into())
        .spawn(move || {
            let mut pipe = std::fs::File::from(fd);
            if let Err(e) = pipe.write_all(&bytes) {
                // A reader that went away (EPIPE) is no fault of the source.
                if e.kind() != io::ErrorKind::BrokenPipe {
                    aterm_log::warn!("wayland: a selection write failed: {e}");
                }
            }
        })
        .map(drop)
}

/// Read `reader` to its end, refusing more than `limit` bytes.
pub fn read_limited(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    reader.take(cap).read_to_end(&mut out)?;
    if out.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("more than {limit} bytes"),
        ));
    }
    Ok(out)
}

/// A MIME type as the protocol takes it.
pub fn mime(s: &str) -> Option<CString> {
    CString::new(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_stops_at_its_limit_and_says_so() {
        assert_eq!(read_limited(&b"abc"[..], 3).unwrap(), b"abc");
        let err = read_limited(&b"abcd"[..], 3).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_detached_write_reaches_the_reader_and_closes() {
        let (mut reader, writer) = io::pipe().unwrap();
        write_detached(OwnedFd::from(writer), b"LANE-42".to_vec()).unwrap();
        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"LANE-42", "and the end of the pipe came");
    }

    fn empty_book() -> Mutex<DeviceBook> {
        Mutex::new(DeviceBook {
            offers: Vec::new(),
            selection: None,
            drag: None,
        })
    }

    #[test]
    fn a_drag_s_events_come_back_for_the_sink_in_the_order_heard() {
        let book = empty_book();
        let motion = [
            EventArg::Uint(7),
            EventArg::Fixed(crate::proxy::Fixed::from_f64(1.5)),
            EventArg::Fixed(crate::proxy::Fixed::from_f64(-2.0)),
        ];
        let heard: Vec<Option<DragEvent>> = [
            (wl_data_device::evt::MOTION, &motion[..]),
            (wl_data_device::evt::DROP, &[][..]),
            (wl_data_device::evt::LEAVE, &[][..]),
        ]
        .into_iter()
        .map(|(op, args)| on_device(&book, Kind::Clipboard, op, args))
        .collect();
        assert_eq!(
            heard,
            [
                Some(DragEvent::Motion {
                    time: 7,
                    x: 1.5,
                    y: -2.0
                }),
                Some(DragEvent::Drop),
                Some(DragEvent::Leave),
            ]
        );
    }

    #[test]
    fn a_selection_and_a_primary_device_make_no_drag_event() {
        let book = empty_book();
        let none = [EventArg::Object(None)];
        assert_eq!(
            on_device(
                &book,
                Kind::Clipboard,
                wl_data_device::evt::SELECTION,
                &none
            ),
            None
        );
        assert_eq!(
            on_device(&book, Kind::Primary, wl_data_device::evt::DROP, &[]),
            None
        );
        assert_eq!(
            on_device(&book, Kind::Clipboard, wl_data_device::evt::DROP, &[]),
            Some(DragEvent::Drop),
            "the same opcode on the clipboard device is a drop"
        );
    }

    #[test]
    fn a_mime_type_with_a_nul_is_refused() {
        assert!(mime("text/plain;charset=utf-8").is_some());
        assert!(mime("a\0b").is_none());
    }
}
