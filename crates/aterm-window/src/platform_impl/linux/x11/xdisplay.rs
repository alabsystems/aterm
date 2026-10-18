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
// docs/AUDIT-third-party-2026-10-15.md, LW.7.)

use std::collections::HashMap;
use std::error::Error;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::{fmt, ptr};

use aterm_x11::proto::xproto::{self, cw, event_mask};
use aterm_x11::proto::{atom, randr, xinput, Reply, Request, ValueList, Void};
use aterm_x11::xrm::Database;

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
    pub cursor_cache: Mutex<HashMap<Option<CursorIcon>, ffi::Cursor>>,
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

        // Load the RandR version.
        let randr_major = xcb
            .extension(randr::NAME)
            .expect("failed to request XRandR version")
            .expect("X server missing XRandR extension")
            .major_opcode;
        let randr_version = xcb
            .send(randr::query_version(randr_major, 1, 3))
            .and_then(|cookie| cookie.reply())
            .expect("failed to query XRandR version");

        let xinput_major = xcb
            .extension(xinput::NAME)
            .expect("Failed to query XInput extension")
            .expect("X server missing XInput extension")
            .major_opcode;

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

        xcb.send_void(xproto::change_window_attributes(
            selector_window,
            &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
        ))
        .ok()?
        .check()
        .ok()?;

        Some(xsettings_screen)
    }

    /// Whether this connection to the X server is broken (the server died,
    /// was killed or reset it): `Some` with the errno Xlib saw when it found
    /// that (`0` when it saw none, or when xcb found it first, through a
    /// request of ours), `None` while the connection holds.
    pub fn connection_lost(&self) -> Option<i32> {
        if IO_ERROR.load(Ordering::SeqCst) {
            return Some(IO_ERROR_ERRNO.load(Ordering::SeqCst));
        }
        self.xcb.as_ref().and_then(|xcb| xcb.status().err()).map(|_| 0)
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
/// are appended in that order. Unlike x11rb, a file's `#include` lines are not followed
/// (aterm-x11's parser skips every directive).
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
    let mut data = if reply.format == 8 && !reply.value.is_empty() {
        reply.value
    } else {
        home.as_ref()
            .and_then(|home| {
                std::fs::read(home.join(".Xresources"))
                    .or_else(|_| std::fs::read(home.join(".Xdefaults")))
                    .ok()
            })
            .unwrap_or_default()
    };
    let merged = match std::env::var_os("XENVIRONMENT") {
        Some(path) => std::fs::read(path).ok(),
        None => {
            let mut name = OsString::from(".Xdefaults-");
            name.push(hostname());
            std::fs::read(home.unwrap_or_default().join(name)).ok()
        },
    };
    if let Some(merged) = merged {
        data.push(b'\n');
        data.extend_from_slice(&merged);
    }
    Ok(Database::parse(&data))
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

/// Set once Xlib has found this process's X connection broken
/// ([`note_io_error`]), with the errno it saw then.
static IO_ERROR: AtomicBool = AtomicBool::new(false);
static IO_ERROR_ERRNO: AtomicI32 = AtomicI32::new(0);

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
/// returns at once, and the event loop ends with the errno Xlib saw
/// ([`XConnection::connection_lost`], `EventLoop::end_if_connection_lost`). A
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

/// The IO error exit handler: notes the loss (again; the first errno is kept)
/// and returns, where Xlib's default would `exit(1)`.
unsafe extern "C" fn io_error_exit(_display: *mut ffi::Display, _data: *mut std::ffi::c_void) {
    note_io_error();
}

/// Note that Xlib found the connection broken, keeping the first errno.
fn note_io_error() {
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let _ = IO_ERROR_ERRNO.compare_exchange(0, errno, Ordering::SeqCst, Ordering::SeqCst);
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
        formatter.write_str(self.description())
    }
}
