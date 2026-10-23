// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party dlopen tables: a system C library opened by versioned soname, its entry points read once into a typed table of function pointers.

use std::ffi::{CStr, c_void};
use std::fmt;
use std::ptr::NonNull;

/// Why a library or one of its symbols could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// This platform has no dynamic loader this crate drives (every cell but Linux).
    Unsupported {
        /// The first name of the list [`Library::open`] was given.
        library: &'static CStr,
    },
    /// No name of the list loaded.
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

impl Error {
    /// Return `true` when this error is `Error::Unsupported`.
    #[inline]
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }

    /// Return `true` when this error is `Error::MissingSymbol`.
    #[inline]
    #[must_use]
    pub const fn is_missing_symbol(&self) -> bool {
        matches!(self, Self::MissingSymbol { .. })
    }
}

/// Encode dynamic-library symbol-table counts (`required_total`, `required_loaded`, `optional_total`, `optional_loaded`) into 16 little-endian bytes.
#[inline]
#[must_use]
pub const fn encode_dl_table_counts_le(
    required_total: u32,
    required_loaded: u32,
    optional_total: u32,
    optional_loaded: u32,
) -> [u8; 16] {
    [
        required_total as u8,
        (required_total >> 8) as u8,
        (required_total >> 16) as u8,
        (required_total >> 24) as u8,
        required_loaded as u8,
        (required_loaded >> 8) as u8,
        (required_loaded >> 16) as u8,
        (required_loaded >> 24) as u8,
        optional_total as u8,
        (optional_total >> 8) as u8,
        (optional_total >> 16) as u8,
        (optional_total >> 24) as u8,
        optional_loaded as u8,
        (optional_loaded >> 8) as u8,
        (optional_loaded >> 16) as u8,
        (optional_loaded >> 24) as u8,
    ]
}

/// Decode 16 little-endian bytes into dynamic-library symbol-table counts.
#[inline]
#[must_use]
pub const fn decode_dl_table_counts_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let req_tot = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let req_ok = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let opt_tot = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let opt_ok = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (req_tot, req_ok, opt_tot, opt_ok)
}

impl std::error::Error for Error {}

/// An opened shared library, kept loaded for the life of the process.
#[derive(Debug)]
pub struct Library {
    handle: NonNull<c_void>,
    soname: &'static CStr,
}

// SAFETY: the handle is an opaque token the loader hands out; dlsym(3) on it is thread-safe, and nothing here mutates through it or ever closes it.
unsafe impl Send for Library {}
// SAFETY: as above; `&Library` only reads symbols.
unsafe impl Sync for Library {}

impl Library {
    /// Open the first of `sonames` that loads.
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
    #[inline]
    #[must_use]
    pub const fn soname(&self) -> &'static CStr {
        self.soname
    }

    /// The address of `symbol`, or `None` when the library does not export it.
    #[inline]
    #[must_use]
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

    /// [`Library::symbol`], with absence an [`Error::MissingSymbol`].
    #[inline]
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

    /// dlopen(3) one name.
    pub(crate) fn open(soname: &CStr) -> Result<NonNull<c_void>, String> {
        // SAFETY: a NUL-terminated name; RTLD_LAZY with no RTLD_GLOBAL is local scope.
        let handle = unsafe { libc::dlopen(soname.as_ptr(), libc::RTLD_LAZY) };
        NonNull::new(handle).ok_or_else(last_error)
    }

    /// dlsym(3) one name on a live handle.
    pub(crate) fn symbol(handle: NonNull<c_void>, symbol: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: a handle dlopen returned and never closed, and a NUL-terminated name.
        NonNull::new(unsafe { libc::dlsym(handle.as_ptr(), symbol.as_ptr()) })
    }

    fn last_error() -> String {
        // SAFETY: dlerror returns null or a NUL-terminated string valid until the next loader call on this thread; it is copied out at once.
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
            /// Read every row from `lib`: a missing required symbol is an error naming it; a missing optional one is `None`.
            /// # Safety
            /// Each row's type must be the C signature of the symbol it names, as `lib` exports it.
            pub unsafe fn load(
                lib: &$crate::Library,
            ) -> ::core::result::Result<Self, $crate::Error> {
                ::core::result::Result::Ok(Self {
                    $( $field: {
                        let p = lib.require($sym)?;
                        // SAFETY: the symbol's address, and the caller vouches for its signature; a data pointer and a fn pointer have the same size on every target this loads on.
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
