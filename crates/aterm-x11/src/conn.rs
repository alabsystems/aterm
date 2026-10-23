// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transport.

use std::ffi::{CStr, c_int, c_uint, c_void};
use std::fmt;
use std::marker::PhantomData;
use std::os::fd::{IntoRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use crate::proto::event::XError;
use crate::proto::xproto::{QueryExtensionReply, Setup, query_extension};
use crate::proto::{ParseError, Reply, Request, Void, big_request};
use crate::sys::{self, Iovec, ProtocolRequest, REQUEST_CHECKED, REQUEST_RAW, Xcb};

/// Why a request, reply or connection failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// libxcb or libX11-xcb is not on this machine (or this platform has no loader).
    Load(aterm_dl::Error),
    /// The connection is broken: libxcb's `XCB_CONN_*` code.
    Connection(c_int),
    /// The server answered the request with an error.
    X11(XError),
    /// The answer did not parse.
    Parse(ParseError),
    /// The request is longer than the server accepts, BIG-REQUESTS included.
    RequestTooLarge,
    /// The client's XID range is spent.
    IdsExhausted,
    /// The setup names no screen of that number.
    NoScreen,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Load(e) => write!(f, "X11 library: {e}"),
            Error::Connection(code) => write!(
                f,
                "X11 connection failed: {} ({code})",
                connection_error_name(*code)
            ),
            Error::X11(e) => write!(
                f,
                "X11 error {} for request {}.{} (value {:#x})",
                e.error_code, e.major_opcode, e.minor_opcode, e.bad_value
            ),
            Error::Parse(e) => write!(f, "{e}"),
            Error::RequestTooLarge => f.write_str("X11 request longer than the server accepts"),
            Error::IdsExhausted => f.write_str("X11 resource ids exhausted"),
            Error::NoScreen => f.write_str("X11 setup has no such screen"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Whether this is the connection's transport failing.
    #[must_use]
    pub const fn is_transport_failure(&self) -> bool {
        matches!(self, Error::Connection(XCB_CONN_ERROR))
    }
}

const XCB_CONN_ERROR: c_int = 1;

impl From<ParseError> for Error {
    fn from(e: ParseError) -> Self {
        Error::Parse(e)
    }
}

fn connection_error_name(code: c_int) -> &'static str {
    match code {
        XCB_CONN_ERROR => "XCB_CONN_ERROR",
        2 => "XCB_CONN_CLOSED_EXT_NOTSUPPORTED",
        3 => "XCB_CONN_CLOSED_MEM_INSUFFICIENT",
        4 => "XCB_CONN_CLOSED_REQ_LEN_EXCEED",
        5 => "XCB_CONN_CLOSED_PARSE_ERR",
        6 => "XCB_CONN_CLOSED_INVALID_SCREEN",
        7 => "XCB_CONN_CLOSED_FDPASSING_FAILED",
        _ => "unknown",
    }
}

/// A libxcb connection.
pub struct Connection {
    xcb: &'static Xcb,
    raw: NonNull<c_void>,
    owned: bool,
    setup: Setup,
    extensions: Mutex<Vec<(&'static [u8], Option<QueryExtensionReply>)>>,
    max_request_bytes: OnceLock<usize>,
}

// SAFETY: libxcb serializes calls on a connection with its own lock, and other fields are Sync.
unsafe impl Send for Connection {}
// SAFETY: as above.
unsafe impl Sync for Connection {}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("raw", &self.raw)
            .field("owned", &self.owned)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Connect to `display` (`$DISPLAY` when `None`).
    pub fn connect(display: Option<&CStr>) -> Result<(Connection, usize), Error> {
        let xcb = sys::xcb().map_err(Error::Load)?;
        let mut screen: c_int = 0;
        let name = display.map_or(std::ptr::null(), CStr::as_ptr);
        // SAFETY: a NUL-terminated name or null, and a live out-parameter.
        let raw = unsafe { (xcb.connect)(name, &mut screen) };
        let raw = NonNull::new(raw).ok_or(Error::Connection(1))?;
        // SAFETY: the connection xcb_connect returned.
        let code = unsafe { (xcb.has_error)(raw.as_ptr()) };
        if code != 0 {
            // SAFETY: an error connection is still disconnected (freed) by xcb_disconnect, once.
            unsafe { (xcb.disconnect)(raw.as_ptr()) };
            return Err(Error::Connection(code));
        }
        // SAFETY: a live connection this value owns from here on.
        let conn = unsafe { Connection::adopt(xcb, raw, true) }?;
        let screen = usize::try_from(screen).map_err(|_| Error::NoScreen)?;
        if screen >= conn.setup.roots.len() {
            return Err(Error::NoScreen);
        }
        Ok((conn, screen))
    }

    /// The XCB connection under an Xlib `Display`, borrowed: dropping the value leaves it open.
    /// # Safety
    /// `display` is a live `Display *` that outlives the returned value.
    pub unsafe fn from_xlib_display(display: NonNull<c_void>) -> Result<Connection, Error> {
        let xcb = sys::xcb().map_err(Error::Load)?;
        let x11_xcb = sys::x11_xcb().map_err(Error::Load)?;
        // SAFETY: the caller's live display.
        let raw = unsafe { (x11_xcb.get_xcb_connection)(display.as_ptr()) };
        let raw = NonNull::new(raw).ok_or(Error::Connection(1))?;
        // SAFETY: Xlib's connection, which the caller keeps alive; not ours to disconnect.
        unsafe { Connection::adopt(xcb, raw, false) }
    }

    /// Wrap a raw `xcb_connection_t *`.
    /// # Safety
    /// `raw` is a live connection that outlives the value.
    pub unsafe fn from_raw(raw: NonNull<c_void>, owned: bool) -> Result<Connection, Error> {
        let xcb = sys::xcb().map_err(Error::Load)?;
        // SAFETY: the caller's contract.
        unsafe { Connection::adopt(xcb, raw, owned) }
    }

    unsafe fn adopt(
        xcb: &'static Xcb,
        raw: NonNull<c_void>,
        owned: bool,
    ) -> Result<Connection, Error> {
        let setup = (|| -> Result<Setup, Error> {
            // SAFETY: a live connection; the setup lives as long as it does and starts with its length.
            unsafe {
                let p = (xcb.get_setup)(raw.as_ptr());
                if p.is_null() {
                    return Err(Error::Connection(1));
                }
                let head = std::slice::from_raw_parts(p, 8);
                let len = Setup::packet_len(head)?;
                Ok(Setup::parse(std::slice::from_raw_parts(p, len))?)
            }
        })();
        let setup = match setup {
            Ok(setup) => setup,
            Err(error) => {
                if owned {
                    // SAFETY: ownership was admitted at entry and no Connection was constructed.
                    unsafe { (xcb.disconnect)(raw.as_ptr()) };
                }
                return Err(error);
            }
        };
        let conn = Connection {
            xcb,
            raw,
            owned,
            setup,
            extensions: Mutex::new(Vec::new()),
            max_request_bytes: OnceLock::new(),
        };
        Ok(conn)
    }

    /// The raw `xcb_connection_t *`, for code that hands it to a C library (Vulkan's WSI).
    pub fn as_raw(&self) -> NonNull<c_void> {
        self.raw
    }

    /// The server's setup.
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// The connection's socket, for a poll loop.
    pub fn file_descriptor(&self) -> RawFd {
        // SAFETY: a live connection.
        unsafe { (self.xcb.get_file_descriptor)(self.raw.as_ptr()) }
    }

    /// The connection's error state, `Ok` while it works.
    pub fn status(&self) -> Result<(), Error> {
        // SAFETY: a live connection.
        match unsafe { (self.xcb.has_error)(self.raw.as_ptr()) } {
            0 => Ok(()),
            code => Err(Error::Connection(code)),
        }
    }

    /// A fresh resource id.
    pub fn generate_id(&self) -> Result<u32, Error> {
        // SAFETY: a live connection.
        let id = unsafe { (self.xcb.generate_id)(self.raw.as_ptr()) };
        if id == u32::MAX {
            self.status()?;
            return Err(Error::IdsExhausted);
        }
        Ok(id)
    }

    /// Write what is queued.
    pub fn flush(&self) -> Result<(), Error> {
        // SAFETY: a live connection.
        if unsafe { (self.xcb.flush)(self.raw.as_ptr()) } > 0 {
            Ok(())
        } else {
            self.status()?;
            Err(Error::Connection(1))
        }
    }

    /// The longest request the server takes, in bytes.
    pub fn maximum_request_bytes(&self) -> usize {
        *self.max_request_bytes.get_or_init(|| {
            // SAFETY: a live connection.
            let words = unsafe { (self.xcb.maximum_request_length)(self.raw.as_ptr()) };
            usize::try_from(words).unwrap_or(0).saturating_mul(4)
        })
    }

    /// Send a request the server answers.
    pub fn send<R: Reply>(&self, req: Request<R>) -> Result<Cookie<'_, R>, Error> {
        let sequence = self.send_raw(req.into_bytes(), false, Vec::new())?;
        Ok(Cookie {
            conn: self,
            sequence,
            reply: PhantomData,
        })
    }

    /// Send a request the server does not answer.
    pub fn send_void(&self, req: Request<Void>) -> Result<VoidCookie<'_>, Error> {
        self.send_void_with_fds(req, Vec::new())
    }

    /// Send a request whose descriptors travel with it.
    pub fn send_void_with_fds(
        &self,
        req: Request<Void>,
        fds: Vec<OwnedFd>,
    ) -> Result<VoidCookie<'_>, Error> {
        let sequence = self.send_raw(req.into_bytes(), true, fds)?;
        Ok(VoidCookie {
            conn: self,
            sequence,
        })
    }

    fn send_raw(&self, bytes: Vec<u8>, is_void: bool, fds: Vec<OwnedFd>) -> Result<u64, Error> {
        let bytes = if bytes.len() / 4 > usize::from(u16::MAX) {
            let big = big_request(&bytes).ok_or(Error::RequestTooLarge)?;
            if big.len() > self.maximum_request_bytes() {
                return Err(Error::RequestTooLarge);
            }
            big
        } else {
            bytes
        };
        let mut iov = [
            Iovec {
                base: std::ptr::null_mut(),
                len: 0,
            },
            Iovec {
                base: std::ptr::null_mut(),
                len: 0,
            },
            Iovec {
                base: bytes.as_ptr().cast_mut().cast(),
                len: bytes.len(),
            },
        ];
        let request = ProtocolRequest {
            count: 1,
            ext: std::ptr::null_mut(),
            opcode: 0,
            isvoid: u8::from(is_void),
        };
        let flags = REQUEST_RAW | REQUEST_CHECKED;
        let sequence = if fds.is_empty() {
            // SAFETY: a live connection; `iov[2]` and `request` outlive the call.
            unsafe { (self.xcb.send_request64)(self.raw.as_ptr(), flags, &mut iov[2], &request) }
        } else {
            let mut raw: Vec<c_int> = fds.into_iter().map(IntoRawFd::into_raw_fd).collect();
            let n = c_uint::try_from(raw.len()).map_err(|_| Error::RequestTooLarge)?;
            // SAFETY: as above, plus descriptors handed to libxcb, which closes them.
            unsafe {
                (self.xcb.send_request_with_fds64)(
                    self.raw.as_ptr(),
                    flags,
                    &mut iov[2],
                    &request,
                    n,
                    raw.as_mut_ptr(),
                )
            }
        };
        if sequence == 0 {
            self.status()?;
            return Err(Error::Connection(1));
        }
        Ok(sequence)
    }

    /// Wait for the reply to request `sequence`.
    pub fn wait_reply<R: Reply>(&self, sequence: u64) -> Result<R, Error> {
        let bytes = self.wait_reply_bytes(sequence)?;
        Ok(R::parse(&bytes)?)
    }

    fn wait_reply_bytes(&self, sequence: u64) -> Result<Vec<u8>, Error> {
        let mut err: *mut u8 = std::ptr::null_mut();
        // SAFETY: a live connection, a sequence this connection issued, and a live out-parameter.
        let reply = unsafe { (self.xcb.wait_for_reply64)(self.raw.as_ptr(), sequence, &mut err) };
        if let Some(e) = NonNull::new(err) {
            // SAFETY: libxcb's malloc'd 32-byte error packet, freed here.
            let parsed = unsafe { take_packet(e, 32) };
            if let Some(r) = NonNull::new(reply) {
                // SAFETY: cannot both be set, but free whatever was.
                unsafe { libc::free(r.as_ptr().cast()) };
            }
            return Err(Error::X11(XError::parse(&parsed)?));
        }
        let Some(reply) = NonNull::new(reply) else {
            self.status()?;
            return Err(Error::Connection(1));
        };
        // SAFETY: a malloc'd reply packet of 32 + 4 * length bytes read whole by libxcb.
        unsafe {
            let head = std::slice::from_raw_parts(reply.as_ptr(), 32);
            let length = u32::from_ne_bytes([head[4], head[5], head[6], head[7]]);
            let len = usize::try_from(length)
                .ok()
                .and_then(|l| l.checked_mul(4))
                .and_then(|l| l.checked_add(32))
                .ok_or(Error::Parse(ParseError::Short))?;
            Ok(take_packet(reply, len))
        }
    }

    /// Wait for a void request's error, if any.
    pub fn check(&self, sequence: u64) -> Result<(), Error> {
        let cookie = sys::VoidCookie {
            sequence: sequence as c_uint,
        };
        // SAFETY: a live connection and a sequence it issued.
        let err = unsafe { (self.xcb.request_check)(self.raw.as_ptr(), cookie) };
        match NonNull::new(err) {
            None => self.status(),
            // SAFETY: libxcb's malloc'd 32-byte error packet, freed here.
            Some(e) => Err(Error::X11(XError::parse(&unsafe { take_packet(e, 32) })?)),
        }
    }

    /// Drop the answer to `sequence` (reply or error) when it comes.
    pub fn discard(&self, sequence: u64) {
        // SAFETY: a live connection and a sequence it issued.
        unsafe { (self.xcb.discard_reply64)(self.raw.as_ptr(), sequence) }
    }

    /// The next event, blocking.
    pub fn wait_for_event(&self) -> Result<Vec<u8>, Error> {
        // SAFETY: a live connection.
        let ev = unsafe { (self.xcb.wait_for_event)(self.raw.as_ptr()) };
        match NonNull::new(ev) {
            // SAFETY: a malloc'd event packet from libxcb.
            Some(ev) => Ok(unsafe { take_event(ev) }),
            None => {
                self.status()?;
                Err(Error::Connection(1))
            }
        }
    }

    /// The next queued event, without blocking.
    pub fn poll_for_event(&self) -> Result<Option<Vec<u8>>, Error> {
        // SAFETY: a live connection.
        let ev = unsafe { (self.xcb.poll_for_event)(self.raw.as_ptr()) };
        match NonNull::new(ev) {
            // SAFETY: a malloc'd event packet from libxcb.
            Some(ev) => Ok(Some(unsafe { take_event(ev) })),
            None => self.status().map(|()| None),
        }
    }

    /// The extension `name`'s opcodes, or `None` when the server lacks it.
    pub fn extension(&self, name: &'static [u8]) -> Result<Option<QueryExtensionReply>, Error> {
        let mut cache = self
            .extensions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, info)) = cache.iter().find(|(n, _)| *n == name) {
            return Ok(*info);
        }
        let reply = self.send(query_extension(name))?.reply()?;
        let info = reply.present.then_some(reply);
        cache.push((name, info));
        Ok(info)
    }
}

impl Connection {
    /// Intern every name in one round trip (all requests first, then the replies), in order.
    pub fn intern_atoms(&self, names: &[&[u8]]) -> Result<Vec<crate::proto::Atom>, Error> {
        let cookies = names
            .iter()
            .map(|name| self.send(crate::proto::xproto::intern_atom(false, name)))
            .collect::<Result<Vec<_>, _>>()?;
        cookies
            .into_iter()
            .map(|c| c.reply().map(|r| r.atom))
            .collect()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: a connection this value owns, disconnected once.
            unsafe { (self.xcb.disconnect)(self.raw.as_ptr()) }
        }
    }
}

unsafe fn take_packet(p: NonNull<u8>, len: usize) -> Vec<u8> {
    // SAFETY: the caller's contract.
    let out = unsafe { std::slice::from_raw_parts(p.as_ptr(), len) }.to_vec();
    // SAFETY: malloc'd by libxcb, freed once.
    unsafe { libc::free(p.as_ptr().cast()) };
    out
}

unsafe fn take_event(p: NonNull<u8>) -> Vec<u8> {
    // SAFETY: at least `xcb_generic_event_t` (36 bytes).
    let head = unsafe { std::slice::from_raw_parts(p.as_ptr(), 36) };
    let mut out = head[..32].to_vec();
    if head[0] & 0x7f == crate::proto::event::GENERIC_EVENT {
        let length = u32::from_ne_bytes([head[4], head[5], head[6], head[7]]);
        if let Some(extra) = usize::try_from(length).ok().and_then(|l| l.checked_mul(4)) {
            // SAFETY: libxcb read the event's `4 * length` bytes after the 36-byte struct.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(p.as_ptr().add(36), extra) });
        }
    }
    // SAFETY: malloc'd by libxcb, freed once.
    unsafe { libc::free(p.as_ptr().cast()) };
    out
}

/// A sent request's pending reply.
pub struct Cookie<'c, R: Reply> {
    conn: &'c Connection,
    sequence: u64,
    reply: PhantomData<fn() -> R>,
}

impl<R: Reply> Cookie<'_, R> {
    /// The request's sequence number.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Wait for and parse the reply.
    pub fn reply(self) -> Result<R, Error> {
        let me = std::mem::ManuallyDrop::new(self);
        me.conn.wait_reply(me.sequence)
    }

    /// Give up the cookie, keeping the reply pending for [`Connection::wait_reply`].
    pub fn detach(self) -> u64 {
        std::mem::ManuallyDrop::new(self).sequence
    }
}

impl<R: Reply> Drop for Cookie<'_, R> {
    fn drop(&mut self) {
        self.conn.discard(self.sequence);
    }
}

/// A sent void request's pending error.
pub struct VoidCookie<'c> {
    conn: &'c Connection,
    sequence: u64,
}

impl VoidCookie<'_> {
    /// The request's sequence number.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Wait until the server has handled the request: its error, or `Ok`.
    pub fn check(self) -> Result<(), Error> {
        let me = std::mem::ManuallyDrop::new(self);
        me.conn.check(me.sequence)
    }

    /// Give up the cookie, keeping the error pending for [`Connection::check`].
    pub fn detach(self) -> u64 {
        std::mem::ManuallyDrop::new(self).sequence
    }
}

impl Drop for VoidCookie<'_> {
    fn drop(&mut self) {
        self.conn.discard(self.sequence);
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn only_the_stream_error_is_the_transport_failing() {
        assert!(Error::Connection(1).is_transport_failure());
        for code in 2..=7 {
            assert!(!Error::Connection(code).is_transport_failure(), "{code}");
        }
        assert!(!Error::RequestTooLarge.is_transport_failure());
    }
}
