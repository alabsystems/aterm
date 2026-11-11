// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-lifetime system frameworks, opened only when a native feature needs them.

use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

pub(crate) struct Framework {
    names: &'static [&'static CStr],
    library: OnceLock<aterm_dl::Library>,
}

impl Framework {
    pub(crate) const fn new(names: &'static [&'static CStr]) -> Self {
        Self {
            names,
            library: OnceLock::new(),
        }
    }

    pub(crate) fn load(&self) {
        let _ = self.library();
    }

    pub(crate) fn class(&self, name: &'static CStr) -> aterm_objc::ClassPtr {
        self.class_using(name, aterm_objc::class)
    }

    fn class_using(
        &self,
        name: &'static CStr,
        lookup: impl FnOnce(&'static CStr) -> aterm_objc::ClassPtr,
    ) -> aterm_objc::ClassPtr {
        self.load();
        lookup(name)
    }

    pub(crate) fn library(&self) -> &aterm_dl::Library {
        self.library.get_or_init(|| {
            aterm_dl::Library::open(self.names).unwrap_or_else(|why| panic!("{why}"))
        })
    }

    fn address(&self, name: &'static CStr) -> NonNull<c_void> {
        self.library()
            .require(name)
            .unwrap_or_else(|why| panic!("{why}"))
    }

    /// # Safety
    /// `T` must be the exact function-pointer type of this exported C symbol.
    pub(crate) unsafe fn function<T: Copy>(&self, name: &'static CStr) -> T {
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        let address = self.address(name).as_ptr();
        // SAFETY: the caller supplies the exact function prototype; Darwin function
        // and data pointers have the same representation and the size was checked.
        unsafe { std::mem::transmute_copy(&address) }
    }

    /// # Safety
    /// This symbol must export a variable of type `T`, aligned and initialized for reads.
    pub(crate) unsafe fn variable<T>(&self, name: &'static CStr) -> Variable<T> {
        Variable(self.address(name).cast())
    }
}

pub(crate) struct Variable<T>(NonNull<T>);
// SAFETY: this only shares a process-lifetime address. Reading is unsafe and must
// satisfy the exported variable's thread and synchronization requirements.
unsafe impl<T> Send for Variable<T> {}
// SAFETY: as above; no safe operation reads or writes the pointee.
unsafe impl<T> Sync for Variable<T> {}

impl<T: Copy> Variable<T> {
    /// # Safety
    /// The variable must be initialized and not concurrently written during this read.
    pub(crate) unsafe fn read(&self) -> T {
        // SAFETY: construction guarantees the type and alignment, the caller synchronizes.
        unsafe { self.0.as_ptr().read() }
    }
}

macro_rules! framework_functions {
    ($framework:ident, $abi:literal; $(
        $(#[$meta:meta])* fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? = $symbol:literal;
    )*) => {$(
        $(#[$meta])*
        pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
            static FUNCTION: std::sync::OnceLock<unsafe extern $abi fn($($ty),*) $(-> $ret)?> =
                std::sync::OnceLock::new();
            // SAFETY: this declaration states the system header's exact prototype.
            let function = FUNCTION.get_or_init(|| unsafe { $framework.function($symbol) });
            // SAFETY: the caller satisfies this API's argument and thread requirements.
            unsafe { function($($arg),*) }
        }
    )*};
}

macro_rules! framework_constants {
    ($framework:ident; $(
        $(#[$meta:meta])* fn $name:ident() -> $ty:ty = $symbol:literal;
    )*) => {$(
        $(#[$meta])*
        pub fn $name() -> $ty {
            static VARIABLE: std::sync::OnceLock<$crate::framework::Variable<$ty>> =
                std::sync::OnceLock::new();
            // SAFETY: the header declares an initialized immutable variable of this type.
            let variable = VARIABLE.get_or_init(|| unsafe { $framework.variable($symbol) });
            // SAFETY: these exported constants are immutable for the framework's lifetime.
            unsafe { variable.read() }
        }
    )*};
}

#[cfg(test)]
#[path = "framework_tests.rs"]
mod tests;
