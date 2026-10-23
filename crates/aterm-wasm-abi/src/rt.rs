// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Internal runtime hooks called by `#[export]` expansions under `aterm_wasm_abi::__rt`.

pub use crate::descriptor::{Part, TypeDesc, record, record_len};
pub use crate::handle::{Class, borrow, borrow_mut, into_raw};
pub use crate::marshal::{Describe, FromAbi, IntoAbi, OptIntoAbi, RefFromAbi, RetWord, Thrown};

/// `aterm_alloc`: [`crate::mem::alloc`], trapping on an invalid layout or exhausted heap.
#[must_use]
pub fn alloc(size: usize, align: usize) -> usize {
    crate::mem::alloc(size, align).unwrap_or_else(|| malloc_failure(size, align))
}

/// `aterm_realloc`: [`crate::mem::realloc`], trapping as [`alloc`].
///
/// # Safety
/// [`crate::mem::realloc`]'s contract.
#[must_use]
pub unsafe fn realloc(ptr: usize, old_size: usize, new_size: usize, align: usize) -> usize {
    // SAFETY: the caller's contract.
    unsafe { crate::mem::realloc(ptr, old_size, new_size, align) }
        .unwrap_or_else(|| malloc_failure(new_size, align))
}

fn malloc_failure(size: usize, align: usize) -> ! {
    panic!("invalid malloc request: {size} bytes at alignment {align}")
}

pub use crate::mem::{free as free_buffer, ret_slot};

/// An export's last step: lower `out`, leave its slot value, and return its word.
///
/// # Errors
/// What the value throws (`Err` of a `Result`).
pub fn finish<R: IntoAbi>(out: R) -> Result<R::Ret, Thrown> {
    let (word, slot) = out.into_abi()?;
    crate::mem::set_ret(slot);
    Ok(word)
}

/// Returns `out`'s word, or `0` with the throw recorded in the return slot.
pub fn returned<W: RetWord>(out: Result<W, Thrown>) -> W {
    match out {
        Ok(word) => word,
        Err(thrown) => {
            crate::mem::set_thrown(thrown);
            W::default()
        }
    }
}

/// Frees a class handle via [`crate::handle::free`], reporting errors unless in a finalizer.
///
/// # Safety
/// [`crate::handle::free`]'s contract.
pub unsafe fn free_export<T: Class>(ptr: usize, from_finalizer: u32) {
    // SAFETY: the caller's contract.
    let out = unsafe { crate::handle::free::<T>(ptr, from_finalizer != 0) };
    if from_finalizer == 0 {
        returned(out);
    }
}

/// Exports `aterm_alloc`, `aterm_realloc`, `aterm_free`, and `aterm_ret_slot`.
#[macro_export]
macro_rules! export_runtime {
    () => {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        const _: () = {
            #[unsafe(no_mangle)]
            extern "C" fn aterm_alloc(size: usize, align: usize) -> usize {
                $crate::__rt::alloc(size, align)
            }

            #[unsafe(no_mangle)]
            unsafe extern "C" fn aterm_realloc(
                ptr: usize,
                old_size: usize,
                new_size: usize,
                align: usize,
            ) -> usize {
                // SAFETY: the glue reallocates only what `aterm_alloc` gave it,
                // with the size it asked for.
                unsafe { $crate::__rt::realloc(ptr, old_size, new_size, align) }
            }

            #[unsafe(no_mangle)]
            unsafe extern "C" fn aterm_free(ptr: usize, size: usize, align: usize) {
                // SAFETY: the glue frees each buffer the module returned once,
                // with the layout its type's lowering names.
                unsafe { $crate::__rt::free_buffer(ptr, size, align) }
            }

            #[unsafe(no_mangle)]
            extern "C" fn aterm_ret_slot() -> usize {
                $crate::__rt::ret_slot()
            }
        };
    };
}

/// The trait impls of an exported class `$t`, for `#[export] struct`.
#[doc(hidden)]
#[macro_export]
macro_rules! __class {
    ($t:ident) => {
        impl $crate::__rt::Class for $t {
            const NAME: &'static str = ::core::stringify!($t);
        }
        impl $crate::__rt::Describe for $t {
            const DESC: $crate::__rt::TypeDesc =
                $crate::__rt::TypeDesc::class(::core::stringify!($t));
        }
        impl $crate::__rt::IntoAbi for $t {
            type Ret = usize;
            fn into_abi(self) -> ::core::result::Result<(usize, u32), $crate::__rt::Thrown> {
                ::core::result::Result::Ok(($crate::__rt::into_raw(self), 0))
            }
        }
        impl $crate::__rt::OptIntoAbi for $t {
            type Ret = usize;
            fn opt_into_abi(
                v: ::core::option::Option<Self>,
            ) -> ::core::result::Result<(usize, u32), $crate::__rt::Thrown> {
                ::core::result::Result::Ok((v.map_or(0, $crate::__rt::into_raw), 0))
            }
        }
    };
}
