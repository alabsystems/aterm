// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opaque handles.

use std::any::TypeId;
use std::cell::{Cell as StdCell, UnsafeCell};
use std::ops::{Deref, DerefMut};

use crate::descriptor::class_tag;
use crate::marshal::Thrown;

const NULL: &str = "null pointer passed to rust";
const ALIASED: &str =
    "recursive use of an object detected which would lead to unsafe aliasing in rust";
const BORROWED: &str = "attempted to take ownership of Rust value while it was borrowed";
const WRONG_CLASS: &str = "an object of another class was passed to rust";

/// An exported class.
pub trait Class: Sized + 'static {
    /// The JS class name.
    const NAME: &'static str;
    /// The name tag every handle of this class carries ([`class_tag`]).
    const TAG: u32 = class_tag(Self::NAME);
}

/// The exact same prefix in every handle, readable without assuming its `T`.
#[repr(C)]
struct Header {
    type_id: TypeId,
    tag: StdCell<u32>,
    /// 0 free, `n > 0` shared borrows, -1 borrowed exclusively.
    borrow: StdCell<isize>,
}

/// A handle's allocation.
#[repr(C)]
pub struct Cell<T> {
    header: Header,
    value: UnsafeCell<T>,
}

/// A shared borrow of a handle's value for one call.
pub struct Ref<'a, T> {
    cell: &'a Cell<T>,
}

/// An exclusive borrow of a handle's value for one call.
pub struct RefMut<'a, T> {
    cell: &'a Cell<T>,
}

/// Box `value` and return its handle, never 0.
#[inline]
#[must_use]
pub fn into_raw<T: Class>(value: T) -> usize {
    Box::into_raw(Box::new(Cell {
        header: Header {
            type_id: TypeId::of::<T>(),
            tag: StdCell::new(T::TAG),
            borrow: StdCell::new(0),
        },
        value: UnsafeCell::new(value),
    })) as usize
}

/// The cell behind `ptr`, checked for null and for its class.
/// # Safety
/// `ptr` is 0 or a handle [`into_raw`] returned, for any class, not yet
/// freed by [`free`].
#[inline]
unsafe fn cell<'a, T: Class>(ptr: usize) -> Result<&'a Cell<T>, Thrown> {
    if ptr == 0 {
        return Err(Thrown::Error(NULL.to_string()));
    }
    // SAFETY: a live handle of any class is a `Cell<_>` whose first field,
    // at offset 0 (`repr(C)`), is exactly this Header, at its own alignment.
    let header = unsafe { &*(ptr as *const Header) };
    if header.type_id != TypeId::of::<T>() || header.tag.get() != T::TAG {
        return Err(Thrown::Error(WRONG_CLASS.to_string()));
    }
    // SAFETY: the Rust type identity proves this handle was made by `into_raw::<T>`.
    Ok(unsafe { &*(ptr as *const Cell<T>) })
}

/// Borrow the value behind `ptr` for a `&self` call.
/// # Safety
/// As [`cell`]: `ptr` is 0 or a live handle of any class.
/// # Errors
/// [`Thrown::Error`]: null, another class, or borrowed exclusively.
#[inline]
pub unsafe fn borrow<'a, T: Class>(ptr: usize) -> Result<Ref<'a, T>, Thrown> {
    // SAFETY: the caller's contract.
    let cell = unsafe { cell::<T>(ptr) }?;
    let n = cell.header.borrow.get();
    if n < 0 || n == isize::MAX {
        return Err(Thrown::Error(ALIASED.to_string()));
    }
    cell.header.borrow.set(n + 1);
    Ok(Ref { cell })
}

/// Borrow the value behind `ptr` for a `&mut self` call.
/// # Safety
/// As [`borrow`].
/// # Errors
/// [`Thrown::Error`]: null, another class, or borrowed at all.
#[inline]
pub unsafe fn borrow_mut<'a, T: Class>(ptr: usize) -> Result<RefMut<'a, T>, Thrown> {
    // SAFETY: the caller's contract.
    let cell = unsafe { cell::<T>(ptr) }?;
    if cell.header.borrow.get() != 0 {
        return Err(Thrown::Error(ALIASED.to_string()));
    }
    cell.header.borrow.set(-1);
    Ok(RefMut { cell })
}

/// Free the handle `ptr`: drop its value and its box.
/// # Safety
/// As [`borrow`]; after an `Ok` the handle is gone and `ptr` must not be used again.
/// # Errors
/// [`Thrown::Error`].
pub unsafe fn free<T: Class>(ptr: usize, from_finalizer: bool) -> Result<(), Thrown> {
    // SAFETY: the caller's contract.
    let cell = unsafe { cell::<T>(ptr) }?;
    if cell.header.borrow.get() != 0 {
        return if from_finalizer {
            Ok(())
        } else {
            Err(Thrown::Error(BORROWED.to_string()))
        };
    }
    cell.header.tag.set(0);
    // SAFETY: `ptr` came from `Box::into_raw` in `into_raw::<T>` (Rust type identity checked).
    drop(unsafe { Box::from_raw(ptr as *mut Cell<T>) });
    Ok(())
}

impl<T> Deref for Ref<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: this guard holds a shared borrow, so no `RefMut` exists.
        unsafe { &*self.cell.value.get() }
    }
}

impl<T> Drop for Ref<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.cell
            .header
            .borrow
            .set(self.cell.header.borrow.get() - 1);
    }
}

impl<T> Deref for RefMut<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: this guard holds the only borrow.
        unsafe { &*self.cell.value.get() }
    }
}

impl<T> DerefMut for RefMut<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: this guard holds the only borrow, and `&mut self` makes this reference unique.
        unsafe { &mut *self.cell.value.get() }
    }
}

impl<T> Drop for RefMut<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.cell.header.borrow.set(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    /// Counts its drops, so a test can tell a freed value from a leaked one.
    struct Probe(Rc<StdCell<u32>>, u32);

    impl Class for Probe {
        const NAME: &'static str = "Probe";
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    struct Other;

    impl Class for Other {
        const NAME: &'static str = "Other";
    }

    fn err(msg: &str) -> Thrown {
        Thrown::Error(msg.to_string())
    }

    fn probe() -> (usize, Rc<StdCell<u32>>) {
        let drops = Rc::new(StdCell::new(0));
        (into_raw(Probe(drops.clone(), 7)), drops)
    }

    #[test]
    fn calls_see_the_value_and_free_drops_it_once() {
        let (h, drops) = probe();
        assert_ne!(h, 0);
        unsafe {
            assert_eq!(borrow::<Probe>(h).unwrap().1, 7);
            borrow_mut::<Probe>(h).unwrap().1 = 8;
            {
                let a = borrow::<Probe>(h).unwrap();
                let b = borrow::<Probe>(h).unwrap();
                assert_eq!(a.1 + b.1, 16, "shared borrows coexist");
            }
            assert_eq!(free::<Probe>(h, false), Ok(()));
        }
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn null_and_another_class_are_refused_before_any_access() {
        let other = into_raw(Other);
        // SAFETY: 0 and live handles only.
        unsafe {
            assert_eq!(borrow::<Probe>(0).err(), Some(err(NULL)));
            assert_eq!(borrow_mut::<Probe>(0).err(), Some(err(NULL)));
            assert_eq!(free::<Probe>(0, false), Err(err(NULL)));
            assert_eq!(borrow::<Probe>(other).err(), Some(err(WRONG_CLASS)));
            assert_eq!(free::<Probe>(other, false), Err(err(WRONG_CLASS)));
            assert_eq!(free::<Other>(other, false), Ok(()));
        }
    }

    /// Distinct, valid struct names whose default FNV-1a/32 tags collide.
    #[test]
    fn matching_class_hashes_do_not_authorize_another_rust_type() {
        #[repr(transparent)]
        struct CUzpSYPuOo(u32);
        #[repr(transparent)]
        struct CPmqmZnSBt(u32);
        impl Class for CUzpSYPuOo {
            const NAME: &'static str = "CUzpSYPuOo";
        }
        impl Class for CPmqmZnSBt {
            const NAME: &'static str = "CPmqmZnSBt";
        }
        assert_eq!(CUzpSYPuOo::TAG, CPmqmZnSBt::TAG, "a genuine collision");
        let ptr = into_raw(CUzpSYPuOo(7));
        // SAFETY: ptr stays a live handle, both types have the same valid
        // representation, and no wrongly admitted value is dereferenced.
        let (shared, exclusive) = unsafe {
            let shared = borrow::<CPmqmZnSBt>(ptr).err();
            let exclusive = borrow_mut::<CPmqmZnSBt>(ptr).err();
            assert_eq!(borrow::<CUzpSYPuOo>(ptr).unwrap().0, 7);
            assert_eq!(free::<CUzpSYPuOo>(ptr, false), Ok(()));
            (shared, exclusive)
        };
        assert_eq!(shared, Some(err(WRONG_CLASS)));
        assert_eq!(exclusive, Some(err(WRONG_CLASS)));
    }

    #[test]
    fn aliasing_borrows_are_refused() {
        let (h, _drops) = probe();
        // SAFETY: `h` is live throughout.
        unsafe {
            let shared = borrow::<Probe>(h).unwrap();
            assert_eq!(borrow_mut::<Probe>(h).err(), Some(err(ALIASED)));
            drop(shared);
            let exclusive = borrow_mut::<Probe>(h).unwrap();
            assert_eq!(borrow::<Probe>(h).err(), Some(err(ALIASED)));
            assert_eq!(borrow_mut::<Probe>(h).err(), Some(err(ALIASED)));
            drop(exclusive);
            assert!(borrow_mut::<Probe>(h).is_ok());
            assert_eq!(free::<Probe>(h, false), Ok(()));
        }
    }

    /// A trap inside a call never drops its guard (`forget` stands in for
    /// the abort): the object is poisoned for good, `free()` throws and
    /// leaks it, the finalizer leaks it quietly, and a sibling is unharmed.
    #[test]
    fn a_trap_mid_call_poisons_only_that_object() {
        let (h, drops) = probe();
        let (sibling, sibling_drops) = probe();
        // SAFETY: both handles stay live (the poisoned one is leaked).
        unsafe {
            std::mem::forget(borrow_mut::<Probe>(h).unwrap());
            assert_eq!(borrow::<Probe>(h).err(), Some(err(ALIASED)));
            assert_eq!(borrow_mut::<Probe>(h).err(), Some(err(ALIASED)));
            assert_eq!(free::<Probe>(h, false), Err(err(BORROWED)));
            assert_eq!(free::<Probe>(h, true), Ok(()));
            assert_eq!(drops.get(), 0, "a poisoned value is never dropped");

            assert_eq!(borrow::<Probe>(sibling).unwrap().1, 7);
            assert_eq!(free::<Probe>(sibling, true), Ok(()));
        }
        assert_eq!(sibling_drops.get(), 1);
    }
}
