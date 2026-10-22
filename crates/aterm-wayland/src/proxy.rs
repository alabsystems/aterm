// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The runtime: proxies, event queues and a dispatcher, over libwayland's
//! own C objects. A request is marshalled by opcode with its arguments in a
//! `union wl_argument` array (`wl_proxy_marshal_array_flags`); an event
//! reaches one Rust closure per proxy through `wl_proxy_add_dispatcher`,
//! its arguments decoded by the message's signature. The generated protocol
//! modules (`crate::protocols`) name the opcodes and argument types; this
//! module does the calls.
//!
//! The display, and objects such as a window's `wl_surface`, may belong to
//! someone else (winit): a [`Proxy`] is a handle, not an owner, and every
//! call that can outlive what it points at is `unsafe`. Objects this crate
//! creates for itself go on a private [`Queue`], so their events are
//! dispatched only when this crate asks, on whichever thread asks.

use std::ffi::{CStr, c_int, c_void};
use std::fmt;
use std::ptr::NonNull;

use crate::iface::{Interface, Message};
use crate::sys::{self, Client, MARSHAL_FLAG_DESTROY, WlArgument, WlArray};

/// Why a call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// libwayland-client is not on this machine (or this platform has no
    /// loader).
    Load(aterm_dl::Error),
    /// The display failed: libwayland's `errno` for it (`EPROTO` after a
    /// protocol error).
    Display(c_int),
    /// libwayland returned no object (out of memory, or a dead display).
    NoObject,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Load(e) => write!(f, "Wayland library: {e}"),
            Error::Display(errno) => write!(f, "Wayland display error (errno {errno})"),
            Error::NoObject => f.write_str("Wayland returned no object"),
        }
    }
}

impl std::error::Error for Error {}

fn client() -> Result<&'static Client, Error> {
    sys::client().map_err(Error::Load)
}

/// `wl_fixed_t`: a signed 24.8 fixed-point number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fixed(pub i32);

impl Fixed {
    /// The nearest fixed value to `v` (saturating).
    #[inline]
    pub fn from_f64(v: f64) -> Fixed {
        Fixed((v * 256.0).round() as i32)
    }

    /// The value.
    #[inline]
    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / 256.0
    }
}

/// What a [`Display::try_flush`] that did not fail left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flushed {
    /// Every queued request is on the socket.
    All,
    /// The socket is full: the rest stays queued for a later flush, once the
    /// socket polls writable.
    Queued,
}

/// A `struct wl_display *`, not owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display(NonNull<c_void>);

// SAFETY: libwayland's display calls are thread-safe.
unsafe impl Send for Display {}
// SAFETY: as above.
unsafe impl Sync for Display {}

impl Display {
    /// Wrap someone else's display.
    ///
    /// # Safety
    /// `raw` remains a live `wl_display *` through every use and drop of
    /// this handle and every queue, proxy, wrapper and listener derived
    /// from it. All queues must be destroyed before the display is closed.
    /// For each private queue, dispatch must be serialized with the use
    /// and destruction of its proxies and listeners; destroying a proxy
    /// does not wait for a callback already running on another thread.
    #[inline]
    pub const unsafe fn from_ptr(raw: NonNull<c_void>) -> Display {
        Display(raw)
    }

    /// The raw pointer.
    #[inline]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The display as a proxy (object 1, `wl_display`).
    #[inline]
    pub const fn proxy(self) -> Proxy {
        Proxy(self.0)
    }

    /// Write queued requests. On failure, the display's fatal error if it has
    /// one, else the errno the write failed with: `EAGAIN` when the socket is
    /// full (the rest stays queued for the next flush, so it is not fatal),
    /// `EPIPE` when the compositor closed it (which libwayland does not record
    /// as the display's error).
    pub fn flush(self) -> Result<(), Error> {
        let c = client()?;
        // SAFETY: a live display (the constructor's contract).
        if unsafe { (c.display_flush)(self.0.as_ptr()) } < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
            return Err(self.error_or(Error::Display(errno)));
        }
        Ok(())
    }

    /// Write queued requests as a client that never blocks on the socket
    /// does: a full socket is [`Flushed::Queued`], not an error, while the
    /// display itself has not failed. Once libwayland's own buffer is full
    /// too it ends the connection with the write's `EAGAIN` as the display's
    /// error (1.22), and that `EAGAIN` is fatal like any other.
    pub fn try_flush(self) -> Result<Flushed, Error> {
        match self.flush() {
            Ok(()) => Ok(Flushed::All),
            Err(Error::Display(errno))
                if self.error().is_none()
                    && std::io::Error::from_raw_os_error(errno).kind()
                        == std::io::ErrorKind::WouldBlock =>
            {
                Ok(Flushed::Queued)
            }
            Err(e) => Err(e),
        }
    }

    /// The display's fatal error, if it has one.
    pub fn error(self) -> Option<Error> {
        let c = client().ok()?;
        // SAFETY: a live display.
        match unsafe { (c.display_get_error)(self.0.as_ptr()) } {
            0 => None,
            errno => Some(Error::Display(errno)),
        }
    }

    fn error_or(self, e: Error) -> Error {
        self.error().unwrap_or(e)
    }

    /// The display's socket.
    pub fn fd(self) -> Result<c_int, Error> {
        let c = client()?;
        // SAFETY: a live display.
        Ok(unsafe { (c.display_get_fd)(self.0.as_ptr()) })
    }
}

/// A display this crate connected to itself; disconnected on drop.
pub struct OwnedDisplay(Display);

impl OwnedDisplay {
    /// Connect to `name` (`$WAYLAND_DISPLAY` when `None`).
    pub fn connect(name: Option<&CStr>) -> Result<OwnedDisplay, Error> {
        let c = client()?;
        let name = name.map_or(std::ptr::null(), CStr::as_ptr);
        // SAFETY: a NUL-terminated name or null.
        let raw = unsafe { (c.display_connect)(name) };
        NonNull::new(raw)
            .map(|p| OwnedDisplay(Display(p)))
            .ok_or(Error::NoObject)
    }

    /// Connect over `fd`, a socket already connected to a compositor (what
    /// `WAYLAND_SOCKET` hands a client). libwayland owns it from here: it is
    /// closed when the display is, or at once when the connection fails.
    pub fn connect_to_fd(fd: std::os::fd::OwnedFd) -> Result<OwnedDisplay, Error> {
        use std::os::fd::IntoRawFd as _;
        let c = client()?;
        // SAFETY: a connected socket whose ownership passes to libwayland.
        let raw = unsafe { (c.display_connect_to_fd)(fd.into_raw_fd()) };
        NonNull::new(raw)
            .map(|p| OwnedDisplay(Display(p)))
            .ok_or(Error::NoObject)
    }

    /// Borrow the raw display as a copyable handle.
    ///
    /// # Safety
    /// This owner must remain alive through every use and drop of the
    /// returned handle and every queue, proxy, wrapper and listener derived
    /// from it. All queues must be destroyed before this owner is dropped.
    /// For each private queue, dispatch must be serialized with the use
    /// and destruction of its proxies and listeners; destroying a proxy
    /// does not wait for a callback already running on another thread.
    #[inline]
    pub const unsafe fn display(&self) -> Display {
        self.0
    }
}

impl Drop for OwnedDisplay {
    fn drop(&mut self) {
        if let Ok(c) = client() {
            // SAFETY: a display this value connected, disconnected once.
            unsafe { (c.display_disconnect)(self.0.as_ptr()) }
        }
    }
}

/// A private event queue on a display; destroyed on drop.
pub struct Queue {
    display: Display,
    raw: NonNull<c_void>,
}

// SAFETY: the queue moves with its owner. The Display admission contract
// requires serialized dispatch and proxy/listener destruction on this
// queue; libwayland's display lock does not join callbacks already running.
unsafe impl Send for Queue {}

impl Queue {
    /// A fresh queue on `display`.
    pub fn new(display: Display) -> Result<Queue, Error> {
        let c = client()?;
        // SAFETY: a live display.
        let raw = unsafe { (c.display_create_queue)(display.as_ptr()) };
        NonNull::new(raw)
            .map(|raw| Queue { display, raw })
            .ok_or(Error::NoObject)
    }

    /// The raw pointer.
    #[inline]
    pub const fn as_ptr(&self) -> *mut c_void {
        self.raw.as_ptr()
    }

    /// A proxy for the display whose new objects land on this queue
    /// (`wl_proxy_create_wrapper` + `wl_proxy_set_queue`), for requests such
    /// as `wl_display.get_registry` that make this crate's own objects.
    pub fn display_wrapper(&self) -> Result<Wrapper, Error> {
        let c = client()?;
        // SAFETY: a live display, wrapped; the wrapper is this queue's.
        unsafe {
            let raw = (c.proxy_create_wrapper)(self.display.as_ptr());
            let raw = NonNull::new(raw).ok_or(Error::NoObject)?;
            (c.proxy_set_queue)(raw.as_ptr(), self.raw.as_ptr());
            Ok(Wrapper(Proxy(raw)))
        }
    }

    /// Send a `wl_display.sync` and dispatch this queue until it answers.
    pub fn roundtrip(&self) -> Result<(), Error> {
        let c = client()?;
        // SAFETY: a live display and this queue.
        let n = unsafe { (c.display_roundtrip_queue)(self.display.as_ptr(), self.raw.as_ptr()) };
        if n < 0 {
            return Err(self.display.error_or(Error::Display(-1)));
        }
        Ok(())
    }

    /// Dispatch this queue's events, reading the socket (blocking) when none
    /// is queued. Safe with other readers of the same display: libwayland's
    /// read protocol hands each queue its own events.
    pub fn dispatch(&self) -> Result<usize, Error> {
        let c = client()?;
        // SAFETY: a live display and this queue.
        let n = unsafe { (c.display_dispatch_queue)(self.display.as_ptr(), self.raw.as_ptr()) };
        usize::try_from(n).map_err(|_| self.display.error_or(Error::Display(-1)))
    }

    /// Dispatch the events already queued, without reading.
    pub fn dispatch_pending(&self) -> Result<usize, Error> {
        let c = client()?;
        // SAFETY: a live display and this queue.
        let n =
            unsafe { (c.display_dispatch_queue_pending)(self.display.as_ptr(), self.raw.as_ptr()) };
        usize::try_from(n).map_err(|_| self.display.error_or(Error::Display(-1)))
    }

    /// Announce a read of the display's socket for this queue, so a caller can
    /// wait on the socket in its own `poll(2)` (libwayland's
    /// `wl_display_prepare_read_queue`). `Ok(None)` when the queue already
    /// holds events: dispatch them ([`Queue::dispatch_pending`]) and ask
    /// again. The intent, once announced, must end in [`ReadIntent::read`] or
    /// be dropped (which cancels it), or every other reader of the display
    /// waits for ever.
    pub fn prepare_read(&self) -> Result<Option<ReadIntent<'_>>, Error> {
        let c = client()?;
        // SAFETY: a live display and this queue.
        let rc =
            unsafe { (c.display_prepare_read_queue)(self.display.as_ptr(), self.raw.as_ptr()) };
        Ok((rc == 0).then(|| ReadIntent {
            display: self.display,
            client: c,
            _queue: std::marker::PhantomData,
        }))
    }
}

/// An announced read of a display's socket ([`Queue::prepare_read`]):
/// [`ReadIntent::read`] reads, a drop cancels.
pub struct ReadIntent<'q> {
    display: Display,
    client: &'static Client,
    _queue: std::marker::PhantomData<&'q Queue>,
}

impl ReadIntent<'_> {
    /// Read what the socket holds into each queue's list
    /// (`wl_display_read_events`), without dispatching. Call it once the
    /// socket polled readable; it does not block on an empty socket (another
    /// reader may have taken the bytes, or the wake was the caller's own).
    pub fn read(self) -> Result<(), Error> {
        let display = self.display;
        let client = self.client;
        std::mem::forget(self);
        // SAFETY: a live display with a read announced by this value, which
        // is consumed here, so it is read or cancelled exactly once.
        if unsafe { (client.display_read_events)(display.as_ptr()) } < 0 {
            return Err(display.error_or(Error::Display(-1)));
        }
        Ok(())
    }
}

impl Drop for ReadIntent<'_> {
    fn drop(&mut self) {
        // SAFETY: a read this value announced and did not perform.
        unsafe { (self.client.display_cancel_read)(self.display.as_ptr()) }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        if let Ok(c) = client() {
            // SAFETY: a queue this value made; every proxy on it is destroyed
            // first (its owners' contract).
            unsafe { (c.event_queue_destroy)(self.raw.as_ptr()) }
        }
    }
}

/// A display wrapper on a private queue; destroyed on drop.
pub struct Wrapper(Proxy);

impl Wrapper {
    /// The wrapper as a proxy, for requests.
    #[inline]
    pub const fn proxy(&self) -> Proxy {
        self.0
    }
}

impl Drop for Wrapper {
    fn drop(&mut self) {
        if let Ok(c) = client() {
            // SAFETY: a wrapper this value made, destroyed once.
            unsafe { (c.proxy_wrapper_destroy)(self.0.as_ptr()) }
        }
    }
}

/// A `struct wl_proxy *`: a handle, not an owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proxy(NonNull<c_void>);

// SAFETY: libwayland's proxy calls are thread-safe.
unsafe impl Send for Proxy {}
// SAFETY: as above.
unsafe impl Sync for Proxy {}

/// One request argument (`union wl_argument`): transparent, so a slice of
/// them is the C array `marshal` hands libwayland.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Arg(WlArgument);

impl Arg {
    #[inline]
    pub const fn int(v: i32) -> Arg {
        Arg(WlArgument { i: v })
    }

    #[inline]
    pub const fn uint(v: u32) -> Arg {
        Arg(WlArgument { u: v })
    }

    #[inline]
    pub const fn fixed(v: Fixed) -> Arg {
        Arg(WlArgument { f: v.0 })
    }

    /// A string (null when `None`), borrowed for the call.
    #[inline]
    pub fn string(v: Option<&CStr>) -> Arg {
        Arg(WlArgument {
            s: v.map_or(std::ptr::null(), CStr::as_ptr),
        })
    }

    /// An object (null when `None`).
    #[inline]
    pub fn object(v: Option<Proxy>) -> Arg {
        Arg(WlArgument {
            o: v.map_or(std::ptr::null_mut(), |p| p.0.as_ptr()),
        })
    }

    /// The placeholder for the object a request creates.
    #[inline]
    pub const fn new_id() -> Arg {
        Arg(WlArgument { n: 0 })
    }

    /// An array, borrowed for the call.
    #[inline]
    pub const fn array(v: &mut WlArray) -> Arg {
        Arg(WlArgument { a: v })
    }

    /// A descriptor, duplicated by libwayland for the send; the caller
    /// keeps (and closes) its own.
    #[inline]
    pub const fn fd(v: c_int) -> Arg {
        Arg(WlArgument { h: v })
    }
}

/// A `wl_array` over borrowed bytes, for a request.
#[inline]
pub const fn borrowed_array(bytes: &[u8]) -> WlArray {
    WlArray {
        size: bytes.len(),
        alloc: bytes.len(),
        data: bytes.as_ptr().cast_mut().cast(),
    }
}

/// One decoded event argument.
#[derive(Clone, Copy, Debug)]
pub enum EventArg<'a> {
    Int(i32),
    Uint(u32),
    Fixed(Fixed),
    Str(Option<&'a CStr>),
    Object(Option<Proxy>),
    /// An object the event created (libwayland made the proxy, on the
    /// receiving proxy's queue); the receiver owns it.
    NewId(Option<Proxy>),
    Array(&'a [u8]),
    /// A descriptor the receiver owns and must close.
    Fd(c_int),
}

impl EventArg<'_> {
    /// The argument as an unsigned integer, if it is one.
    #[inline]
    pub const fn uint(&self) -> Option<u32> {
        match *self {
            EventArg::Uint(v) => Some(v),
            _ => None,
        }
    }

    /// The argument as a signed integer, if it is one.
    #[inline]
    pub const fn int(&self) -> Option<i32> {
        match *self {
            EventArg::Int(v) => Some(v),
            _ => None,
        }
    }
}

/// The most arguments one event carries here (the protocols' longest is 5).
pub const MAX_ARGS: usize = 16;

/// What a dispatcher closure receives: the proxy, the event's opcode, and
/// its arguments.
pub type Handler = dyn FnMut(Proxy, u32, &[EventArg<'_>]) + Send;

/// The closure behind a proxy's dispatcher; freed on drop, which must come
/// after the proxy is destroyed.
pub struct Listener(*mut Box<Handler>);

// SAFETY: the closure is `Send`; the pointer is this value's alone.
unsafe impl Send for Listener {}
// SAFETY: a shared `&Listener` can do nothing (it has no `&self` methods), so
// it may be shared with any thread; only its owner's drop touches the closure.
unsafe impl Sync for Listener {}

impl Drop for Listener {
    fn drop(&mut self) {
        // SAFETY: made by `Box::into_raw` in `Proxy::listen`, freed once.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

impl Proxy {
    /// Wrap someone else's proxy (a window's `wl_surface`).
    ///
    /// # Safety
    /// `raw` is a live `wl_proxy *` for as long as the value is used.
    #[inline]
    pub const unsafe fn from_ptr(raw: NonNull<c_void>) -> Proxy {
        Proxy(raw)
    }

    /// The raw pointer.
    #[inline]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The object's version.
    ///
    /// # Safety
    /// The proxy is live.
    pub unsafe fn version(self) -> u32 {
        // SAFETY: the caller's contract; a proxy exists only once the
        // library is loaded.
        client().map_or(0, |c| unsafe { (c.proxy_get_version)(self.0.as_ptr()) })
    }

    /// The object's id.
    ///
    /// # Safety
    /// The proxy is live.
    pub unsafe fn id(self) -> u32 {
        // SAFETY: as for `version`.
        client().map_or(0, |c| unsafe { (c.proxy_get_id)(self.0.as_ptr()) })
    }

    /// The object's interface name.
    ///
    /// # Safety
    /// The proxy is live; the name lives as long as its interface table.
    pub unsafe fn class(self) -> Option<&'static CStr> {
        let c = client().ok()?;
        // SAFETY: the caller's contract.
        let p = unsafe { (c.proxy_get_class)(self.0.as_ptr()) };
        // SAFETY: a NUL-terminated name from a static interface table.
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) })
    }

    /// Send request `opcode` with `args` (one per signature argument). With
    /// `new`, the request creates an object of that interface and version
    /// (its `new_id` argument is [`Arg::new_id`]), returned on this proxy's
    /// queue. With `destroy`, the proxy is destroyed after the send (a
    /// destructor request).
    ///
    /// # Safety
    /// The proxy is live, `args` match the request's signature, and a
    /// destroyed proxy is not used again.
    pub unsafe fn marshal(
        self,
        opcode: u32,
        args: &mut [Arg],
        new: Option<(&'static Interface, u32)>,
        destroy: bool,
    ) -> Option<Proxy> {
        let c = client().ok()?;
        let (iface, version) = match new {
            Some((i, v)) => (std::ptr::from_ref(i), v),
            // SAFETY: the caller's live proxy.
            None => (std::ptr::null(), unsafe {
                (c.proxy_get_version)(self.0.as_ptr())
            }),
        };
        let flags = if destroy { MARSHAL_FLAG_DESTROY } else { 0 };
        // SAFETY: the caller's contract; `Arg` is `union wl_argument`.
        let out = unsafe {
            (c.proxy_marshal_array_flags)(
                self.0.as_ptr(),
                opcode,
                iface,
                version,
                flags,
                args.as_mut_ptr().cast(),
            )
        };
        NonNull::new(out).map(Proxy)
    }

    /// Destroy the proxy without a request (an object the protocol ends with
    /// an event, such as `wl_callback`).
    ///
    /// # Safety
    /// The proxy is live and is not used again.
    pub unsafe fn destroy(self) {
        if let Ok(c) = client() {
            // SAFETY: the caller's contract.
            unsafe { (c.proxy_destroy)(self.0.as_ptr()) }
        }
    }

    /// Move the proxy's future events to `queue`.
    ///
    /// # Safety
    /// The proxy is live and outlived by the queue.
    pub unsafe fn set_queue(self, queue: &Queue) {
        if let Ok(c) = client() {
            // SAFETY: the caller's contract.
            unsafe { (c.proxy_set_queue)(self.0.as_ptr(), queue.as_ptr()) }
        }
    }

    /// Deliver the proxy's events to `handler`. A proxy takes one listener,
    /// once.
    ///
    /// # Safety
    /// The proxy is live and was made by this crate (a foreign proxy has its
    /// owner's listener), and the returned [`Listener`] outlives it (drop the
    /// listener only after destroying the proxy).
    pub unsafe fn listen(
        self,
        handler: impl FnMut(Proxy, u32, &[EventArg<'_>]) + Send + 'static,
    ) -> Result<Listener, Error> {
        let c = client()?;
        let boxed: Box<Box<Handler>> = Box::new(Box::new(handler));
        let data = Box::into_raw(boxed);
        // SAFETY: a live proxy without a listener; `data` lives until the
        // returned Listener drops.
        let rc = unsafe {
            (c.proxy_add_dispatcher)(
                self.0.as_ptr(),
                dispatch,
                data.cast_const().cast(),
                std::ptr::null_mut(),
            )
        };
        let listener = Listener(data);
        if rc != 0 {
            return Err(Error::Display(rc));
        }
        Ok(listener)
    }
}

/// The argument kinds of `msg`'s signature, in order (its version digits
/// and `?` marks left out).
fn arg_kinds(msg: &Message) -> impl Iterator<Item = u8> + '_ {
    msg.signature()
        .to_bytes()
        .iter()
        .copied()
        .filter(|c| matches!(c, b'i' | b'u' | b'f' | b's' | b'o' | b'n' | b'a' | b'h'))
}

/// Close the descriptors of an event no handler receives: libwayland hands
/// them to the dispatcher, so nothing else would.
///
/// # Safety
/// `msg` and `args` are the event's, as libwayland passed them, and no
/// handler has seen them.
unsafe fn close_fds(msg: &Message, args: *const WlArgument) {
    for (i, kind) in arg_kinds(msg).enumerate() {
        if kind != b'h' {
            continue;
        }
        // SAFETY: one argument per signature argument, and this one is a
        // descriptor.
        let fd = unsafe { (*args.add(i)).h };
        #[cfg(unix)]
        {
            use std::os::fd::{FromRawFd, OwnedFd};
            // SAFETY: a descriptor this side owns, closed once here.
            drop(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        // No libwayland, so no event, off unix.
        #[cfg(not(unix))]
        let _ = fd;
    }
}

/// The one dispatcher: decode the arguments by the signature, call the
/// proxy's closure. A panic in the closure is caught here (it cannot unwind
/// into C) and logged. An event it cannot hand to a closure has its
/// descriptors closed.
unsafe extern "C" fn dispatch(
    data: *const c_void,
    target: *mut c_void,
    opcode: u32,
    msg: *const Message,
    args: *mut WlArgument,
) -> c_int {
    // SAFETY: libwayland's table for this event.
    let Some(msg) = (unsafe { msg.as_ref() }) else {
        return 0;
    };
    let (Some(proxy), Some(handler)) = (
        NonNull::new(target),
        // SAFETY: the pointer `listen` registered, live until its Listener
        // drops after the proxy.
        unsafe { data.cast_mut().cast::<Box<Handler>>().as_mut() },
    ) else {
        // SAFETY: the event's own arguments, which no handler saw.
        unsafe { close_fds(msg, args) };
        return 0;
    };
    if arg_kinds(msg).count() > MAX_ARGS {
        // SAFETY: as above.
        unsafe { close_fds(msg, args) };
        return 0;
    }
    let mut decoded = [EventArg::Uint(0); MAX_ARGS];
    let mut n = 0;
    for kind in arg_kinds(msg) {
        let Some(slot) = decoded.get_mut(n) else {
            return 0;
        };
        // SAFETY: libwayland passes one argument per signature argument.
        let a = unsafe { *args.add(n) };
        // SAFETY: each union field read is the one the signature names.
        *slot = unsafe {
            match kind {
                b'i' => EventArg::Int(a.i),
                b'u' => EventArg::Uint(a.u),
                b'f' => EventArg::Fixed(Fixed(a.f)),
                b's' => EventArg::Str((!a.s.is_null()).then(|| CStr::from_ptr(a.s))),
                b'o' => EventArg::Object(NonNull::new(a.o).map(Proxy)),
                b'n' => EventArg::NewId(NonNull::new(a.o).map(Proxy)),
                b'a' => EventArg::Array(match a.a.as_ref() {
                    Some(arr) if !arr.data.is_null() => {
                        std::slice::from_raw_parts(arr.data.cast::<u8>(), arr.size)
                    }
                    _ => &[],
                }),
                _ => EventArg::Fd(a.h),
            }
        };
        n += 1;
    }
    let call = std::panic::AssertUnwindSafe(|| handler(Proxy(proxy), opcode, &decoded[..n]));
    if std::panic::catch_unwind(call).is_err() {
        aterm_log::warn!("wayland: an event handler panicked; event dropped");
    }
    0
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{ErrorKind, Read};
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::iface::{NULLS, TypeRef};

    /// A socket pair: the far end's descriptor for an event, and the near
    /// end, which reads end-of-file once the far end is closed.
    fn pair() -> (c_int, UnixStream) {
        let (near, far) = UnixStream::pair().expect("socketpair");
        near.set_nonblocking(true).expect("nonblocking");
        (far.into_raw_fd(), near)
    }

    /// Whether the far end of `near` is closed (it reads end-of-file; an
    /// open, idle far end would block).
    fn far_closed(near: &mut UnixStream) -> bool {
        match near.read(&mut [0u8; 1]) {
            Ok(0) => true,
            Err(e) if e.kind() == ErrorKind::WouldBlock => false,
            other => panic!("read: {other:?}"),
        }
    }

    static KEYMAP: Message = Message::new(c"keymap", c"uhu", &NULLS);
    static WIDE_TYPES: [TypeRef; 17] = [
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static WIDE: Message = Message::new(c"wide", c"uuuuuuuuuuuuuuuuh", &WIDE_TYPES);

    #[test]
    fn an_event_no_handler_takes_has_its_descriptor_closed() {
        let (fd, mut near) = pair();
        let mut args = [
            WlArgument { u: 1 },
            WlArgument { h: fd },
            WlArgument { u: 2 },
        ];
        let target = NonNull::<c_void>::dangling().as_ptr();
        // SAFETY: the event's arguments match its signature; no handler.
        unsafe {
            dispatch(
                std::ptr::null(),
                target,
                0,
                &raw const KEYMAP,
                args.as_mut_ptr(),
            )
        };
        assert!(far_closed(&mut near), "the descriptor stayed open");
    }

    #[test]
    fn an_event_past_max_args_has_its_descriptor_closed() {
        let (fd, mut near) = pair();
        let mut args = [WlArgument { u: 0 }; MAX_ARGS + 1];
        args[MAX_ARGS] = WlArgument { h: fd };
        let mut handler: Box<Handler> = Box::new(|_, _, _| panic!("past MAX_ARGS, never called"));
        let data = std::ptr::from_mut(&mut handler).cast_const().cast();
        let target = NonNull::<c_void>::dangling().as_ptr();
        // SAFETY: as above, with a handler that is not reached.
        unsafe { dispatch(data, target, 0, &raw const WIDE, args.as_mut_ptr()) };
        assert!(far_closed(&mut near), "the descriptor stayed open");
    }

    /// The negative control: a handler receives the descriptor, and owns it.
    #[test]
    fn a_handled_event_leaves_its_descriptor_to_the_handler() {
        let (fd, mut near) = pair();
        let mut args = [
            WlArgument { u: 1 },
            WlArgument { h: fd },
            WlArgument { u: 2 },
        ];
        let got = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = std::sync::Arc::clone(&got);
        let mut handler: Box<Handler> = Box::new(move |_, _, args| {
            if let [_, EventArg::Fd(fd), _] = args {
                *seen.lock().expect("lock") = Some(*fd);
            }
        });
        let data = std::ptr::from_mut(&mut handler).cast_const().cast();
        let target = NonNull::<c_void>::dangling().as_ptr();
        // SAFETY: as above.
        unsafe { dispatch(data, target, 0, &raw const KEYMAP, args.as_mut_ptr()) };
        assert_eq!(*got.lock().expect("lock"), Some(fd));
        assert!(
            !far_closed(&mut near),
            "the dispatcher closed a handled descriptor"
        );
        // SAFETY: the descriptor the handler was given, closed by its owner.
        drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        assert!(far_closed(&mut near));
    }
}
