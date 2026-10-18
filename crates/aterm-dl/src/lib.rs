// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party dlopen tables: a system C library opened by versioned soname,
//! its entry points read once into a typed table of function pointers.
//!
//! aterm links against no windowing library on Linux. The X11 and Wayland
//! clients (`aterm-x11`, `aterm-wayland`, `aterm-xkb`) open `libxcb`,
//! `libX11`, `libwayland-client` and `libxkbcommon` at run time, so one binary
//! runs on a desktop that lacks either stack, and so the C handles wgpu's
//! Vulkan and aterm's EGL surfaces need are the system's own
//! (docs/AUDIT-third-party-2026-10-15.md, linux-windowing LW.6). This crate is
//! the loader under them, in place of dlib and the dlopen halves of x11-dl,
//! wayland-sys, xkbcommon-dl and tiny-xlib.
//!
//! * [`Library::open`] tries a list of sonames in order (the versioned name a
//!   runtime package installs first, the bare development symlink last) and
//!   reports every name it tried with the loader's own reason.
//! * [`table!`] declares a struct of `unsafe extern "C" fn` pointers, one row
//!   per symbol, and its `load` reads them all or names the first one missing.
//!   Rows in an `optional` block are `Option`s, for entry points a library
//!   gained in a later release.
//! * A library is never closed. The windowing libraries register atexit
//!   handlers and hand out pointers (interface tables, connections) that
//!   outlive any one caller; a `dlclose` would leave them dangling. The handle
//!   is dropped and the mapping stays, which is what the crates this replaces
//!   did too (dlib keeps its `Library` in a process-lifetime static).
//!
//! Off Linux there is nothing to open: [`Library::open`] answers
//! [`Error::Unsupported`], so a caller compiled on every cell degrades as it
//! would on a Linux machine without the library.

use std::ffi::{CStr, c_void};
use std::fmt;
use std::ptr::NonNull;

/// Why a library or one of its symbols could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// This platform has no dynamic loader this crate drives (every cell but
    /// Linux). `library` is the first soname asked for.
    Unsupported {
        /// The first name of the list [`Library::open`] was given.
        library: &'static CStr,
    },
    /// No name of the list loaded. `tried` holds each name with the loader's
    /// reason for it (`dlerror`'s text), in the order tried.
    NotFound {
        /// `(soname, reason)` per name tried.
        tried: Vec<(&'static CStr, String)>,
    },
    /// The library loaded but lacks a required symbol.
    MissingSymbol {
        /// The soname that loaded.
        library: &'static CStr,
        /// The symbol it does not export.
        symbol: &'static CStr,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported { library } => write!(
                f,
                "{}: no dynamic loader on this platform",
                library.to_string_lossy()
            ),
            Error::NotFound { tried } => {
                f.write_str("no library loaded:")?;
                for (i, (name, reason)) in tried.iter().enumerate() {
                    let sep = if i == 0 { " " } else { "; " };
                    write!(f, "{sep}{} ({reason})", name.to_string_lossy())?;
                }
                Ok(())
            }
            Error::MissingSymbol { library, symbol } => write!(
                f,
                "{} lacks {}",
                library.to_string_lossy(),
                symbol.to_string_lossy()
            ),
        }
    }
}

impl std::error::Error for Error {}

/// An opened shared library, kept loaded for the life of the process.
#[derive(Debug)]
pub struct Library {
    handle: NonNull<c_void>,
    soname: &'static CStr,
}

// SAFETY: the handle is an opaque token the loader hands out; dlsym(3) on it
// is thread-safe, and nothing here mutates through it or ever closes it.
unsafe impl Send for Library {}
// SAFETY: as above; `&Library` only reads symbols.
unsafe impl Sync for Library {}

impl Library {
    /// Open the first of `sonames` that loads.
    ///
    /// `RTLD_LAZY` (functions bind on first call) and local scope: the
    /// library's symbols do not join the global namespace another library
    /// resolves against, as libloading's default was.
    pub fn open(sonames: &'static [&'static CStr]) -> Result<Library, Error> {
        #[cfg(target_os = "linux")]
        {
            let mut tried = Vec::with_capacity(sonames.len());
            for &soname in sonames {
                match sys::open(soname) {
                    Ok(handle) => return Ok(Library { handle, soname }),
                    Err(reason) => tried.push((soname, reason)),
                }
            }
            Err(Error::NotFound { tried })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(Error::Unsupported {
                library: sonames.first().copied().unwrap_or(c"(none)"),
            })
        }
    }

    /// The soname that loaded.
    pub fn soname(&self) -> &'static CStr {
        self.soname
    }

    /// The address of `symbol`, or `None` when the library does not export
    /// it. Function or data: the caller knows which and what type.
    pub fn symbol(&self, symbol: &CStr) -> Option<NonNull<c_void>> {
        #[cfg(target_os = "linux")]
        {
            sys::symbol(self.handle, symbol)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (self.handle, symbol);
            None
        }
    }

    /// [`Library::symbol`], with absence an [`Error::MissingSymbol`]. The
    /// table rows read through it.
    pub fn require(&self, symbol: &'static CStr) -> Result<NonNull<c_void>, Error> {
        self.symbol(symbol).ok_or(Error::MissingSymbol {
            library: self.soname,
            symbol,
        })
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::ffi::{CStr, c_void};
    use std::ptr::NonNull;

    /// dlopen(3) one name. The error is dlerror(3)'s text, read at once on
    /// this thread (glibc keeps it per thread).
    pub(crate) fn open(soname: &CStr) -> Result<NonNull<c_void>, String> {
        // SAFETY: a NUL-terminated name; RTLD_LAZY with no RTLD_GLOBAL is
        // local scope. Loading runs the library's constructors, which is
        // what opening it means.
        let handle = unsafe { libc::dlopen(soname.as_ptr(), libc::RTLD_LAZY) };
        NonNull::new(handle).ok_or_else(last_error)
    }

    /// dlsym(3) one name on a live handle.
    pub(crate) fn symbol(handle: NonNull<c_void>, symbol: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: a handle dlopen returned and never closed, and a
        // NUL-terminated name. A null answer is absence: none of the symbols
        // aterm reads can legitimately sit at address zero.
        NonNull::new(unsafe { libc::dlsym(handle.as_ptr(), symbol.as_ptr()) })
    }

    fn last_error() -> String {
        // SAFETY: dlerror returns null or a NUL-terminated string valid until
        // the next loader call on this thread; it is copied out at once.
        let text = unsafe { libc::dlerror() };
        if text.is_null() {
            return "the loader gave no reason".to_owned();
        }
        // SAFETY: non-null, NUL-terminated (above).
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Declare a table of entry points read from one [`Library`].
///
/// Each row is `field = c"symbol": <fn pointer type>;`, the type spelled in
/// full (`unsafe extern "C" fn(..) -> ..`, variadic rows included), so a row
/// reads against the C prototype it transcribes. Rows in a trailing
/// `optional { .. }` block become `Option` fields that are `None` when the
/// library predates the symbol.
///
/// The generated `load` is `unsafe`: the caller vouches that each row's type
/// is the symbol's real C signature, which nothing at run time can check.
///
/// ```
/// aterm_dl::table! {
///     /// Two libc entry points.
///     pub struct Clock {
///         getpid = c"getpid": unsafe extern "C" fn() -> i32;
///     }
///     optional {
///         gettid = c"gettid": unsafe extern "C" fn() -> i32;
///     }
/// }
/// static LIBC: &[&std::ffi::CStr] = &[c"libc.so.6"];
/// if let Ok(lib) = aterm_dl::Library::open(LIBC) {
///     // SAFETY: both rows are glibc's prototypes.
///     let clock = unsafe { Clock::load(&lib) }.unwrap();
///     // SAFETY: getpid takes nothing and cannot fail.
///     assert!(unsafe { (clock.getpid)() } > 0);
/// }
/// ```
#[macro_export]
macro_rules! table {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $( $(#[$fmeta:meta])* $field:ident = $sym:literal : $fty:ty; )*
        }
        $( optional {
            $( $(#[$ometa:meta])* $ofield:ident = $osym:literal : $oty:ty; )*
        } )?
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy)]
        $vis struct $name {
            $( $(#[$fmeta])* pub $field: $fty, )*
            $($( $(#[$ometa])* pub $ofield: ::core::option::Option<$oty>, )*)?
        }

        impl $name {
            /// Read every row from `lib`: a missing required symbol is an
            /// error naming it; a missing optional one is `None`.
            ///
            /// # Safety
            /// Each row's type must be the C signature of the symbol it
            /// names, as `lib` exports it.
            pub unsafe fn load(
                lib: &$crate::Library,
            ) -> ::core::result::Result<Self, $crate::Error> {
                ::core::result::Result::Ok(Self {
                    $( $field: {
                        let p = lib.require($sym)?;
                        // SAFETY: the symbol's address, and the caller
                        // vouches for its signature; a data pointer and a fn
                        // pointer have the same size on every target this
                        // loads on.
                        unsafe {
                            ::core::mem::transmute::<*mut ::core::ffi::c_void, $fty>(p.as_ptr())
                        }
                    }, )*
                    $($( $ofield: match lib.symbol($osym) {
                        ::core::option::Option::Some(p) => ::core::option::Option::Some(
                            // SAFETY: as for the required rows.
                            unsafe {
                                ::core::mem::transmute::<*mut ::core::ffi::c_void, $oty>(
                                    p.as_ptr(),
                                )
                            },
                        ),
                        ::core::option::Option::None => ::core::option::Option::None,
                    }, )*)?
                })
            }
        }
    };
}
