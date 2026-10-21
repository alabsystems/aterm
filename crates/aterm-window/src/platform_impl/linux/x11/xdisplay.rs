// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/xdisplay.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (Dropped the last-seen
// server timestamp cache: its one reader was the deleted startup-notification flow. A broken
// connection ends the event loop instead of the process: `install_io_error_exit_handler`.
// The libraries, the XCB connection under Xlib's display and the resource database are
// first-party `aterm-x11`'s, where upstream used x11-dl and x11rb:
// docs/AUDIT-third-party-2026-10-15.md, LW.7. With no XSETTINGS manager there is no window
// to watch, and none is asked for: upstream selected events on window 0 and took the
// BadWindow, which the lane's X11 wire trace showed on every start, X11-TRACE.)

use std::collections::HashMap;
use std::error::Error;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::{fmt, ptr};

use aterm_x11::proto::xproto::{self, cw, event_mask};
use aterm_x11::proto::{atom, randr, xinput, Reply, Request, ValueList, Void};
use aterm_x11::xrm::{self, Database};

use crate::window::CursorIcon;

use super::atoms::Atoms;
use super::ffi;
use super::monitor::MonitorHandle;
use super::{VoidCookie, X11Error};

/// The XKEYBOARD extension's name; aterm-x11 encodes none of its requests (Xlib's
/// `XkbSelectEvents` selects its events), so only its event base is looked up.
pub const XKB_EXTENSION_NAME: &[u8] = b"XKEYBOARD";

/// A connection to an X server.
pub struct XConnection {
    pub xlib: &'static ffi::Xlib,
    pub xcursor: &'static ffi::Xcursor,

    // TODO(notgull): I'd like to remove this, but apparently Xlib and Xinput2 are tied together
    // for some reason.
    pub xinput2: &'static ffi::Xi,

    pub display: *mut ffi::Display,

    /// The XCB connection under the display, borrowed from Xlib.
    ///
    /// The `Option` ensures that we can drop it before we close the `Display`.
    xcb: Option<aterm_x11::Connection>,

    /// The atoms used by `winit`.
    ///
    /// This is a large structure, so I've elected to Box it to make accessing the fields of
    /// this struct easier. Feel free to unbox it if you like kicking puppies.
    atoms: Box<Atoms>,

    /// The index of the default screen.
    default_screen: usize,

    /// List of monitor handles.
    pub monitor_handles: Mutex<Option<Vec<MonitorHandle>>>,

    /// The resource database.
    database: RwLock<Database>,

    /// RandR's major opcode.
    randr_major: u8,

    /// RandR version.
    randr_version: (u32, u32),

    /// XInput's major opcode.
    xinput_major: u8,

    /// Atom for the XSettings screen.
    xsettings_screen: Option<xproto::Atom>,

    pub latest_error: Mutex<Option<XError>>,
    pub cursor_cache: Mutex<HashMap<CursorIcon, ffi::Cursor>>,
}

unsafe impl Send for XConnection {}
unsafe impl Sync for XConnection {}

pub type XErrorHandler = ffi::XErrorHandler;

impl XConnection {
    pub fn new(error_handler: XErrorHandler) -> Result<XConnection, XNotSupported> {
        // opening the libraries
        let xlib = ffi::xlib().map_err(aterm_x11::Error::Load)?;
        let xcursor = ffi::xcursor().map_err(aterm_x11::Error::Load)?;
        let xinput2 = ffi::xi().map_err(aterm_x11::Error::Load)?;

        unsafe { (xlib.XInitThreads)() };
        unsafe { (xlib.XSetErrorHandler)(error_handler) };

        // calling XOpenDisplay
        let display = unsafe {
            let display = (xlib.XOpenDisplay)(ptr::null());
            if display.is_null() {
                return Err(XNotSupported::XOpenDisplayFailed);
            }
            display
        };

        // SAFETY: `display` is the live display just opened, and the handlers
        // only note the loss.
        unsafe { install_io_error_exit_handler(xlib, display) };

        // The XCB connection under the display, which Xlib keeps open until
        // `XCloseDisplay` (after this value is dropped: see `Drop`).
        // SAFETY: the display was just opened and outlives `xcb`.
        let xcb =
            unsafe { aterm_x11::Connection::from_xlib_display(NonNull::new_unchecked(display)) }
                .map_err(|e| XNotSupported::XcbConversionError(Arc::new(e)))?;

        // Get the default screen.
        let default_screen = unsafe { (xlib.XDefaultScreen)(display) } as usize;

        // Load the database.
        let database =
            load_database(&xcb).map_err(|e| XNotSupported::XcbConversionError(Arc::new(e)))?;

        // What the backend needs of the server, asked before anything uses it: RandR 1.3 or
        // later (the monitors, their modes and scale), XInput 2 (every pointer, touch and
        // focus event) and XKB (the keyboard). A server without one is refused here, by name,
        // where upstream panicked further in (the Linux lane, 2026-10-20: under Xvfb's
        // `-extension RANDR` aterm-gui died of "X server missing XRandR extension").
        let failed = |e: aterm_x11::Error| XNotSupported::XcbConversionError(Arc::new(e));
        let present = |name: &'static [u8]| -> Result<u8, XNotSupported> {
            match xcb.extension(name).map_err(failed)? {
                Some(info) => Ok(info.major_opcode),
                None => Err(XNotSupported::MissingExtension(Requirement::of(name))),
            }
        };
        let randr_major = present(randr::NAME)?;
        let randr_version = xcb
            .send(randr::query_version(randr_major, 1, 3))
            .and_then(|cookie| cookie.reply())
            .map_err(failed)?;
        if (randr_version.major_version, randr_version.minor_version) < (1, 3) {
            return Err(XNotSupported::MissingExtension(Requirement::RandR13));
        }
        let xinput_major = present(xinput::NAME)?;
        // A server whose XInputExtension predates version 2 does not know XIQueryVersion and
        // answers it with an X error (`Request`): it lacks XInput 2, which is what it says.
        let xi_version = match xcb
            .send(xinput::xi_query_version(xinput_major, 2, 3))
            .and_then(|cookie| cookie.reply())
        {
            Ok(version) => version,
            Err(aterm_x11::Error::X11(_)) => {
                return Err(XNotSupported::MissingExtension(Requirement::XInput2))
            },
            Err(e) => return Err(failed(e)),
        };
        if xi_version.major_version < 2 {
            return Err(XNotSupported::MissingExtension(Requirement::XInput2));
        }
        present(XKB_EXTENSION_NAME)?;

        let xsettings_screen = Self::new_xsettings_screen(&xcb, default_screen);
        if xsettings_screen.is_none() {
            tracing::warn!("error setting XSETTINGS; Xft options won't reload automatically")
        }

        // Fetch atoms.
        let atoms = Atoms::new(&xcb).map_err(|e| XNotSupported::XcbConversionError(Arc::new(e)))?;

        Ok(XConnection {
            xlib,
            xcursor,
            xinput2,
            display,
            xcb: Some(xcb),
            atoms: Box::new(atoms),
            default_screen,
            latest_error: Mutex::new(None),
            monitor_handles: Mutex::new(None),
            database: RwLock::new(database),
            cursor_cache: Default::default(),
            randr_major,
            randr_version: (randr_version.major_version, randr_version.minor_version),
            xinput_major,
            xsettings_screen,
        })
    }

    fn new_xsettings_screen(
        xcb: &aterm_x11::Connection,
        default_screen: usize,
    ) -> Option<xproto::Atom> {
        // Fetch the _XSETTINGS_S[screen number] atom.
        let name = format!("_XSETTINGS_S{default_screen}");
        let xsettings_screen =
            xcb.send(xproto::intern_atom(false, name.as_bytes())).ok()?.reply().ok()?.atom;

        // Get PropertyNotify events from the XSETTINGS window.
        // TODO: The XSETTINGS window here can change. In the future, listen for DestroyNotify on
        // this window in order to accommodate for a changed window here.
        let selector_window =
            xcb.send(xproto::get_selection_owner(xsettings_screen)).ok()?.reply().ok()?.owner;
        // No manager owns the selection (a bare X server, many window managers): nothing to
        // watch. Selecting on window 0 would only earn a BadWindow.
        if selector_window == aterm_x11::proto::NONE {
            return None;
        }

        xcb.send_void(xproto::change_window_attributes(
            selector_window,
            &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
        ))
        .ok()?
        .check()
        .ok()?;

        Some(xsettings_screen)
    }

    /// Whether this connection to the X server is broken: Xlib found it so (its IO error
    /// handlers ran, [`install_io_error_exit_handler`]) or libxcb has an error. Two loads, so
    /// the event loop asks before each event it hands on.
    pub fn is_broken(&self) -> bool {
        IO_ERROR.load(Ordering::SeqCst)
            || self.xcb.as_ref().is_some_and(|xcb| xcb.status().is_err())
    }

    /// How this connection ended ([`ConnectionEnd`]), `None` while it holds and the event
    /// loop's poll saw its socket neither hang up nor fail (`hung_up`).
    pub fn connection_end(&self, hung_up: bool) -> Option<ConnectionEnd> {
        let status = self.xcb.as_ref().map_or(Ok(()), aterm_x11::Connection::status);
        ConnectionEnd::of(status, IO_ERROR.load(Ordering::SeqCst), hung_up, |seen| {
            socket_end(self.xcb_connection().file_descriptor(), seen)
        })
    }

    /// Checks whether an error has been triggered by the previous function calls.
    #[inline]
    pub fn check_errors(&self) -> Result<(), XError> {
        let error = self.latest_error.lock().unwrap().take();
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }

    #[inline]
    pub fn randr_version(&self) -> (u32, u32) {
        self.randr_version
    }

    /// RandR's major opcode, which its requests carry.
    #[inline]
    pub fn randr_major(&self) -> u8 {
        self.randr_major
    }

    /// XInput's major opcode, which its requests carry.
    #[inline]
    pub fn xinput_major(&self) -> u8 {
        self.xinput_major
    }

    /// Get the underlying XCB connection.
    #[inline]
    pub fn xcb_connection(&self) -> &aterm_x11::Connection {
        self.xcb.as_ref().expect("xcb_connection somehow called after drop?")
    }

    /// Send a request the server does not answer; its cookie holds its error.
    #[inline]
    pub fn send_void(&self, request: Request<Void>) -> Result<VoidCookie<'_>, X11Error> {
        self.xcb_connection().send_void(request).map_err(Into::into)
    }

    /// Send a request and wait for its reply.
    #[inline]
    pub fn request<R: Reply>(&self, request: Request<R>) -> Result<R, X11Error> {
        Ok(self.xcb_connection().send(request)?.reply()?)
    }

    /// Get the list of atoms.
    #[inline]
    pub fn atoms(&self) -> &Atoms {
        &self.atoms
    }

    /// Get the index of the default screen.
    #[inline]
    pub fn default_screen_index(&self) -> usize {
        self.default_screen
    }

    /// Get the default screen.
    #[inline]
    pub fn default_root(&self) -> &xproto::Screen {
        &self.xcb_connection().setup().roots[self.default_screen]
    }

    /// Get the resource database.
    #[inline]
    pub fn database(&self) -> RwLockReadGuard<'_, Database> {
        self.database.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Reload the resource database.
    #[inline]
    pub fn reload_database(&self) -> Result<(), super::X11Error> {
        let database = load_database(self.xcb_connection())?;
        *self.database.write().unwrap_or_else(|e| e.into_inner()) = database;
        Ok(())
    }

    /// Get the atom for Xsettings.
    #[inline]
    pub fn xsettings_screen(&self) -> Option<xproto::Atom> {
        self.xsettings_screen
    }
}

impl fmt::Debug for XConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.display.fmt(f)
    }
}

/// The resource database as Xlib's `XrmGetDatabase` would build it, the order x11rb's
/// `resource_manager::new_from_default` reads its sources in: the `RESOURCE_MANAGER` property
/// of the first screen's root window (what `xrdb` sets), else `~/.Xresources`, else
/// `~/.Xdefaults`; then, merged after it, the file `$XENVIRONMENT` names, else
/// `~/.Xdefaults-<hostname>`. A later entry of equal precedence wins, so each file's lines
/// are appended in that order. A file's `#include "path"` lines are followed from its own
/// directory, as x11rb followed them ([`Database::parse_file`]); the property's are not, its
/// text being `xrdb`'s output, which cpp has already expanded.
fn load_database(conn: &aterm_x11::Connection) -> Result<Database, X11Error> {
    let root = conn.setup().roots.first().map_or(0, |screen| screen.root);
    // x11rb asks for at most 100,000,000 words of the property, as Xlib does.
    let reply = conn
        .send(xproto::get_property(
            false,
            root,
            atom::RESOURCE_MANAGER,
            atom::STRING,
            0,
            100_000_000,
        ))?
        .reply()?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let file = |path: PathBuf| {
        std::fs::read(&path).ok().map(|data| Database::parse_file(&data, &xrm::include_dir(&path)))
    };
    let mut database = if reply.format == 8 && !reply.value.is_empty() {
        Database::parse(&reply.value)
    } else {
        home.as_ref()
            .and_then(|home| {
                file(home.join(".Xresources")).or_else(|| file(home.join(".Xdefaults")))
            })
            .unwrap_or_default()
    };
    let merged = match std::env::var_os("XENVIRONMENT") {
        Some(path) => file(PathBuf::from(path)),
        None => {
            let mut name = OsString::from(".Xdefaults-");
            name.push(hostname());
            file(home.unwrap_or_default().join(name))
        },
    };
    if let Some(merged) = merged {
        database.append(merged);
    }
    Ok(database)
}

/// This machine's host name: uname(2)'s node name, which glibc's `gethostname` returns.
pub(crate) fn hostname() -> OsString {
    // SAFETY: a zeroed utsname is a valid out-parameter (byte arrays).
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: a live, exclusively borrowed utsname.
    if unsafe { libc::uname(&mut uts) } != 0 {
        return OsString::new();
    }
    // SAFETY: uname(2) succeeded, so `nodename` holds a NUL-terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(uts.nodename.as_ptr()) };
    OsString::from_vec(name.to_bytes().to_vec())
}

/// How a connection to the X server ended ([`XConnection::connection_end`]).
#[derive(Debug)]
pub enum ConnectionEnd {
    /// Its transport failed: libxcb's `XCB_CONN_ERROR` (a read found end of file, or a read or
    /// write on the socket failed), Xlib's IO error, or the event loop's poll saw the socket
    /// hang up or fail. The server died, was killed or restarted; the error is what the socket
    /// says of its end ([`socket_end`]).
    Transport(std::io::Error),
    /// libxcb closed the connection for a reason of its own, the server still there (a request
    /// longer than the server takes, memory, an answer that did not parse): its error.
    Closed(aterm_x11::Error),
}

impl ConnectionEnd {
    /// The end of a connection whose libxcb `status` is this, whose Xlib has (`xlib_io_error`)
    /// or has not found it broken, and whose socket the loop's poll saw hang up or fail
    /// (`hung_up`); `None` while it holds. libxcb's word comes first: it is the one that read
    /// and wrote the socket. A failed transport's error is `transport`'s, what the socket says,
    /// told which of the three found it failed.
    pub(crate) fn of(
        status: Result<(), aterm_x11::Error>,
        xlib_io_error: bool,
        hung_up: bool,
        transport: impl FnOnce(&'static str) -> std::io::Error,
    ) -> Option<ConnectionEnd> {
        let seen = match status {
            Err(error) if !error.is_transport_failure() => {
                return Some(ConnectionEnd::Closed(error))
            },
            Err(_) => "a read or write on the X server's socket failed (XCB_CONN_ERROR)",
            Ok(()) if xlib_io_error => "Xlib found the X server's connection broken (its IO error)",
            Ok(()) if hung_up => "the X server's socket hung up or failed (the event loop's poll)",
            Ok(()) => return None,
        };
        Some(ConnectionEnd::Transport(transport(seen)))
    }
}

/// What the X server's socket says of its end, once its transport failed: a pending socket
/// error (`SO_ERROR`; `ECONNRESET` when the server died with requests of ours unread and
/// nothing has read the socket since), else what a peek finds: nothing at all is end of file,
/// the server's side closed, and an error is that error. When the socket still holds unread
/// bytes, or says nothing, the word of whichever found the transport failed (`seen`) stands.
/// Xlib's `errno` is not used: it is whatever the last call left (with Xvfb killed under
/// aterm-gui it named no connection's end in any lane run, 2026-10-17), and end of file sets
/// none.
pub(crate) fn socket_end(fd: std::os::raw::c_int, seen: &'static str) -> std::io::Error {
    let mut pending: std::os::raw::c_int = 0;
    let mut len = std::mem::size_of::<std::os::raw::c_int>() as libc::socklen_t;
    // SAFETY: `pending` and `len` are live locals of the sizes `getsockopt` is told.
    let read = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut pending as *mut std::os::raw::c_int).cast(),
            &mut len,
        )
    };
    if read == 0 && pending != 0 {
        return std::io::Error::from_raw_os_error(pending);
    }
    let mut byte = 0u8;
    // SAFETY: one byte into a live local; a peek that does not block takes nothing off the
    // socket.
    let peeked = unsafe {
        libc::recv(fd, (&mut byte as *mut u8).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT)
    };
    let error = std::io::Error::last_os_error();
    if peeked == 0 {
        return std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "end of file: the X server closed the connection",
        );
    }
    if peeked < 0 && error.kind() != std::io::ErrorKind::WouldBlock {
        return error;
    }
    std::io::Error::other(seen)
}

/// Set once Xlib has found this process's X connection broken ([`note_io_error`]).
static IO_ERROR: AtomicBool = AtomicBool::new(false);

/// Make a broken connection end the event loop instead of the process.
///
/// When Xlib finds its connection broken (`_XIOError`) it calls the IO error
/// handler and then the IO error EXIT handler. Both defaults end the process:
/// the exit handler's is `exit(1)`, and Xlib's IO error handler, which prints
/// `X connection to :0 broken (explicit kill or server shutdown).`, ends it
/// before the exit handler runs (the Linux lane, libX11 1.8.7, 2026-10-17:
/// with only the exit handler replaced, aterm-gui still printed that and
/// exited 1 when Xvfb was killed). No application code runs after either, so
/// a crashed or restarted X server ended aterm with nothing saved. libX11 1.7 (2020) added
/// `XSetIOErrorExitHandler`, an optional row of aterm-x11's Xlib table. Where
/// it exists, both handlers become ones that
/// note the loss and return (as Mutter installs them for its Xwayland); Xlib
/// then marks the display broken (`XlibDisplayIOError`), every later call on it
/// returns at once, and the event loop ends with the connection's end
/// ([`XConnection::connection_end`], `EventLoop::end_if_connection_lost`). A
/// libX11 older than 1.7 has no exit handler: its defaults stay, and so does
/// its `exit(1)`, for a returning IO error handler would end in it anyway.
///
/// # Safety
/// `display` is a display `XOpenDisplay` returned and nothing has closed.
unsafe fn install_io_error_exit_handler(xlib: &ffi::Xlib, display: *mut ffi::Display) {
    let Some(set) = xlib.XSetIOErrorExitHandler else {
        return;
    };
    // SAFETY: `display` is live (the caller's contract), and `io_error_exit`
    // has the handler's signature and ignores its user data.
    unsafe { set(display, Some(io_error_exit), ptr::null_mut()) };
    // SAFETY: `io_error` has `XIOErrorHandler`'s signature. The handler is
    // process-wide; this process opens this one display.
    unsafe { (xlib.XSetIOErrorHandler)(Some(io_error)) };
}

/// The IO error handler: notes the loss and returns, where Xlib's own would
/// `exit(1)`. Called by Xlib on whichever thread's call found the connection
/// broken, before [`io_error_exit`].
unsafe extern "C" fn io_error(_display: *mut ffi::Display) -> std::os::raw::c_int {
    note_io_error();
    0
}

/// The IO error exit handler: notes the loss (again) and returns, where Xlib's
/// default would `exit(1)`.
unsafe extern "C" fn io_error_exit(_display: *mut ffi::Display, _data: *mut std::ffi::c_void) {
    note_io_error();
}

/// Note that Xlib found the connection broken.
fn note_io_error() {
    IO_ERROR.store(true, Ordering::SeqCst);
}

impl Drop for XConnection {
    #[inline]
    fn drop(&mut self) {
        self.xcb = None;
        unsafe { (self.xlib.XCloseDisplay)(self.display) };
    }
}

/// Error triggered by xlib.
#[derive(Debug, Clone)]
pub struct XError {
    pub description: String,
    pub error_code: u8,
    pub request_code: u8,
    pub minor_code: u8,
}

impl Error for XError {}

impl fmt::Display for XError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(
            formatter,
            "X error: {} (code: {}, request code: {}, minor code: {})",
            self.description, self.error_code, self.request_code, self.minor_code
        )
    }
}

/// Error returned if this system doesn't have XLib or can't create an X connection.
#[derive(Clone, Debug)]
pub enum XNotSupported {
    /// Failed to load one or several shared libraries.
    LibraryOpenError(aterm_x11::Error),

    /// Connecting to the X server with `XOpenDisplay` failed.
    XOpenDisplayFailed, // TODO: add better message.

    /// We encountered an error while converting the connection to XCB.
    XcbConversionError(Arc<dyn Error + Send + Sync + 'static>),

    /// The server lacks an extension the backend needs, or has too old a version of it.
    MissingExtension(Requirement),
}

/// An X extension the X11 backend cannot run without ([`XNotSupported::MissingExtension`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    /// RANDR at version 1.3 or later: the monitors, their modes and scale.
    RandR13,
    /// XInputExtension at version 2: every pointer, touch and focus event.
    XInput2,
    /// XKEYBOARD: the keyboard's layout and state.
    Xkb,
}

impl Requirement {
    /// The requirement an extension's name stands for.
    fn of(name: &[u8]) -> Requirement {
        if name == randr::NAME {
            Requirement::RandR13
        } else if name == xinput::NAME {
            Requirement::XInput2
        } else {
            Requirement::Xkb
        }
    }

    /// The extension's name, as the server lists it.
    pub fn name(self) -> &'static str {
        match self {
            Requirement::RandR13 => "RANDR",
            Requirement::XInput2 => "XInputExtension",
            Requirement::Xkb => "XKEYBOARD",
        }
    }

    /// What the backend needs of it.
    pub fn needs(self) -> &'static str {
        match self {
            Requirement::RandR13 => "RandR 1.3 or later",
            Requirement::XInput2 => "XInput 2",
            Requirement::Xkb => "XKB",
        }
    }
}

impl From<aterm_x11::Error> for XNotSupported {
    #[inline]
    fn from(err: aterm_x11::Error) -> XNotSupported {
        XNotSupported::LibraryOpenError(err)
    }
}

impl XNotSupported {
    fn description(&self) -> &'static str {
        match self {
            XNotSupported::LibraryOpenError(_) => "Failed to load one of xlib's shared libraries",
            XNotSupported::XOpenDisplayFailed => "Failed to open connection to X server",
            XNotSupported::XcbConversionError(_) => "Failed to convert Xlib connection to XCB",
            XNotSupported::MissingExtension(_) => "The X server lacks an extension aterm needs",
        }
    }
}

impl Error for XNotSupported {
    #[inline]
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            XNotSupported::LibraryOpenError(ref err) => Some(err),
            XNotSupported::XcbConversionError(ref err) => Some(&**err),
            _ => None,
        }
    }
}

impl fmt::Display for XNotSupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            XNotSupported::MissingExtension(r) => write!(
                formatter,
                "the X server has no {} extension (or too old a one); aterm's X11 backend needs {}",
                r.name(),
                r.needs()
            ),
            _ => formatter.write_str(self.description()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Write as _};
    use std::os::unix::io::AsRawFd as _;
    use std::os::unix::net::UnixStream;

    use super::{socket_end, ConnectionEnd};

    /// A socket with nothing to say: the witness's word stands.
    fn witness(seen: &'static str) -> std::io::Error {
        std::io::Error::other(seen)
    }

    fn said(end: Option<ConnectionEnd>) -> String {
        match end {
            Some(ConnectionEnd::Transport(why)) => why.to_string(),
            other => panic!("not a failed transport: {other:?}"),
        }
    }

    /// Only a failed transport is the display's loss: libxcb's stream error,
    /// Xlib's IO error, or a socket the poll saw hang up. libxcb's own close
    /// is its error, and a connection nothing found broken has not ended.
    #[test]
    fn only_a_failed_transport_is_the_end_of_the_server() {
        let transport = aterm_x11::Error::Connection(1);
        let too_long = aterm_x11::Error::Connection(4);
        let ended = |status, xlib, hung_up| ConnectionEnd::of(status, xlib, hung_up, witness);
        for (xlib, hung_up) in [(false, false), (true, false), (false, true)] {
            assert!(matches!(
                ended(Err(transport.clone()), xlib, hung_up),
                Some(ConnectionEnd::Transport(_))
            ));
            assert!(matches!(
                ended(Err(too_long.clone()), xlib, hung_up),
                Some(ConnectionEnd::Closed(aterm_x11::Error::Connection(4)))
            ));
        }
        // Each witness is named: libxcb's first, then Xlib's, then the poll's.
        assert!(said(ended(Err(transport.clone()), true, true)).contains("XCB_CONN_ERROR"));
        assert!(said(ended(Ok(()), true, true)).contains("Xlib"));
        assert!(said(ended(Ok(()), false, true)).contains("poll"));
        assert!(ended(Ok(()), false, false).is_none());
    }

    /// What the socket says of a dead server: end of file once it closed,
    /// `ECONNRESET` when it closed on requests of ours it never read. A live
    /// socket, or one still holding the server's last bytes, says nothing of
    /// its own (the control: no end of file is made up).
    #[test]
    fn the_socket_says_how_the_server_went() {
        let (ours, server) = UnixStream::pair().expect("socketpair");
        let quiet = socket_end(ours.as_raw_fd(), "the witness");
        assert_eq!(quiet.kind(), ErrorKind::Other, "{quiet}");
        assert_eq!(quiet.to_string(), "the witness");
        drop(server);
        let closed = socket_end(ours.as_raw_fd(), "the witness");
        assert_eq!(closed.kind(), ErrorKind::UnexpectedEof, "{closed}");

        let (mut ours, server) = UnixStream::pair().expect("socketpair");
        ours.write_all(b"a request").expect("write");
        drop(server);
        let reset = socket_end(ours.as_raw_fd(), "the witness");
        assert_eq!(reset.raw_os_error(), Some(libc::ECONNRESET), "{reset}");

        let (ours, mut server) = UnixStream::pair().expect("socketpair");
        server.write_all(b"an event").expect("write");
        drop(server);
        let unread = socket_end(ours.as_raw_fd(), "the witness");
        assert_eq!(unread.to_string(), "the witness");
    }
}
