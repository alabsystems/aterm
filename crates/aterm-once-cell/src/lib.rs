// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `once_cell` implementation backed by [`std::sync::OnceLock`], [`std::sync::Mutex`], and [`core::cell::Cell`].

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

/// Single-threaded, non-blocking [`OnceCell`](unsync::OnceCell) and [`Lazy`](unsync::Lazy).
pub mod unsync {
    use core::cell::{Cell, OnceCell as CoreOnceCell};
    use core::fmt;
    use core::ops::{Deref, DerefMut};
    use core::panic::{RefUnwindSafe, UnwindSafe};

    /// Single-threaded cell that can be written to at most once.
    pub struct OnceCell<T>(CoreOnceCell<T>);

    impl<T: RefUnwindSafe + UnwindSafe> RefUnwindSafe for OnceCell<T> {}

    impl<T> Default for OnceCell<T> {
        #[inline]
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.get() {
                Some(v) => f.debug_tuple("OnceCell").field(v).finish(),
                None => f.write_str("OnceCell(Uninit)"),
            }
        }
    }

    impl<T: Clone> Clone for OnceCell<T> {
        fn clone(&self) -> OnceCell<T> {
            match self.get() {
                Some(value) => OnceCell::with_value(value.clone()),
                None => OnceCell::new(),
            }
        }

        fn clone_from(&mut self, source: &Self) {
            match (self.get_mut(), source.get()) {
                (Some(this), Some(source)) => this.clone_from(source),
                _ => *self = source.clone(),
            }
        }
    }

    impl<T: PartialEq> PartialEq for OnceCell<T> {
        fn eq(&self, other: &Self) -> bool {
            self.get() == other.get()
        }
    }

    impl<T: Eq> Eq for OnceCell<T> {}

    impl<T> From<T> for OnceCell<T> {
        #[inline]
        fn from(value: T) -> Self {
            OnceCell::with_value(value)
        }
    }

    impl<T> OnceCell<T> {
        /// Creates a new empty cell.
        #[inline]
        pub const fn new() -> OnceCell<T> {
            OnceCell(CoreOnceCell::new())
        }

        /// Creates a new initialized cell.
        #[inline]
        pub fn with_value(value: T) -> OnceCell<T> {
            let cell = CoreOnceCell::new();
            let _ = cell.set(value);
            OnceCell(cell)
        }

        /// Gets a shared reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.0.get()
        }

        /// Gets a mutable reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get_mut(&mut self) -> Option<&mut T> {
            self.0.get_mut()
        }

        /// Sets the contents of this cell, returning `Err(value)` if already full.
        #[inline]
        pub fn set(&self, value: T) -> Result<(), T> {
            match self.try_insert(value) {
                Ok(_) => Ok(()),
                Err((_, value)) => Err(value),
            }
        }

        /// Sets the contents of this cell and returns a reference to the stored value.
        pub fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
            let mut value = Some(value);
            let res = self.get_or_init(|| {
                value
                    .take()
                    .unwrap_or_else(|| unreachable!("the initialiser runs at most once"))
            });
            match value {
                None => Ok(res),
                Some(value) => Err((res, value)),
            }
        }

        /// Gets the contents of the cell, initializing it with `f` if empty.
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> T,
        {
            self.0.get_or_init(f)
        }

        /// Gets the contents of the cell, initializing it with fallible `f` if empty.
        ///
        /// # Panics
        /// Panics if `f` re-entrantly initializes `self`.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<T, E>,
        {
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let val = f()?;
            assert!(self.0.set(val).is_ok(), "reentrant init");
            Ok(self
                .get()
                .unwrap_or_else(|| unreachable!("just initialised")))
        }

        /// Takes the value out of the cell, leaving it empty.
        #[inline]
        pub fn take(&mut self) -> Option<T> {
            self.0.take()
        }

        /// Consumes the cell, returning the wrapped value.
        #[inline]
        pub fn into_inner(self) -> Option<T> {
            self.0.into_inner()
        }
    }

    /// Single-threaded value initialized on first access.
    /// If the initializer panics, the `Lazy` is poisoned and subsequent accesses panic.
    pub struct Lazy<T, F = fn() -> T> {
        cell: OnceCell<T>,
        init: Cell<Option<F>>,
    }

    impl<T, F: RefUnwindSafe> RefUnwindSafe for Lazy<T, F> where OnceCell<T>: RefUnwindSafe {}

    impl<T: fmt::Debug, F> fmt::Debug for Lazy<T, F> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Lazy")
                .field("cell", &self.cell)
                .field("init", &"..")
                .finish()
        }
    }

    impl<T, F> Lazy<T, F> {
        /// Creates a new lazy value with the given initializing function.
        #[inline]
        pub const fn new(init: F) -> Lazy<T, F> {
            Lazy {
                cell: OnceCell::new(),
                init: Cell::new(Some(init)),
            }
        }

        /// Consumes this `Lazy`, returning `Ok(value)` if initialized or `Err(f)` if not.
        pub fn into_value(this: Lazy<T, F>) -> Result<T, F> {
            let cell = this.cell;
            let init = this.init;
            cell.into_inner().ok_or_else(|| {
                init.take()
                    .unwrap_or_else(|| panic!("Lazy instance has previously been poisoned"))
            })
        }
    }

    impl<T, F: FnOnce() -> T> Lazy<T, F> {
        /// Forces evaluation and returns a shared reference to the result.
        pub fn force(this: &Lazy<T, F>) -> &T {
            this.cell.get_or_init(|| match this.init.take() {
                Some(f) => f(),
                None => panic!("Lazy instance has previously been poisoned"),
            })
        }

        /// Forces evaluation and returns a mutable reference to the result.
        pub fn force_mut(this: &mut Lazy<T, F>) -> &mut T {
            if this.cell.get_mut().is_none() {
                let value = match this.init.get_mut().take() {
                    Some(f) => f(),
                    None => panic!("Lazy instance has previously been poisoned"),
                };
                let _ = this.cell.set(value);
            }
            this.cell
                .get_mut()
                .unwrap_or_else(|| unreachable!("just initialised"))
        }

        /// Gets a shared reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get(this: &Lazy<T, F>) -> Option<&T> {
            this.cell.get()
        }

        /// Gets a mutable reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
            this.cell.get_mut()
        }
    }

    impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
        type Target = T;
        #[inline]
        fn deref(&self) -> &T {
            Lazy::force(self)
        }
    }

    impl<T, F: FnOnce() -> T> DerefMut for Lazy<T, F> {
        #[inline]
        fn deref_mut(&mut self) -> &mut T {
            Lazy::force_mut(self)
        }
    }

    impl<T: Default> Default for Lazy<T> {
        /// Creates a new lazy value using `Default` as the initialising function.
        #[inline]
        fn default() -> Lazy<T> {
            Lazy::new(T::default)
        }
    }
}

/// Thread-safe, blocking [`OnceCell`](sync::OnceCell) and [`Lazy`](sync::Lazy).
pub mod sync {
    use core::convert::Infallible;
    use core::fmt;
    use core::ops::{Deref, DerefMut};
    use std::sync::{Mutex, OnceLock, PoisonError};

    /// Thread-safe cell that can be written to at most once.
    pub struct OnceCell<T> {
        inner: OnceLock<T>,
        /// Held across a user initialiser so that exactly one runs.
        gate: Mutex<()>,
    }

    impl<T> Default for OnceCell<T> {
        #[inline]
        fn default() -> OnceCell<T> {
            OnceCell::new()
        }
    }

    impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.get() {
                Some(v) => f.debug_tuple("OnceCell").field(v).finish(),
                None => f.write_str("OnceCell(Uninit)"),
            }
        }
    }

    impl<T: Clone> Clone for OnceCell<T> {
        fn clone(&self) -> OnceCell<T> {
            match self.get() {
                Some(value) => Self::with_value(value.clone()),
                None => Self::new(),
            }
        }

        fn clone_from(&mut self, source: &Self) {
            match (self.get_mut(), source.get()) {
                (Some(this), Some(source)) => this.clone_from(source),
                _ => *self = source.clone(),
            }
        }
    }

    impl<T> From<T> for OnceCell<T> {
        #[inline]
        fn from(value: T) -> Self {
            Self::with_value(value)
        }
    }

    impl<T: PartialEq> PartialEq for OnceCell<T> {
        fn eq(&self, other: &OnceCell<T>) -> bool {
            self.get() == other.get()
        }
    }

    impl<T: Eq> Eq for OnceCell<T> {}

    impl<T> OnceCell<T> {
        /// Creates a new empty cell.
        #[inline]
        pub const fn new() -> OnceCell<T> {
            OnceCell {
                inner: OnceLock::new(),
                gate: Mutex::new(()),
            }
        }

        /// Creates a new initialized cell.
        #[inline]
        pub fn with_value(value: T) -> OnceCell<T> {
            let cell = OnceCell::new();
            let _ = cell.inner.set(value);
            cell
        }

        /// Gets a shared reference to the underlying value without blocking, or `None` if empty.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.inner.get()
        }

        /// Gets a mutable reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get_mut(&mut self) -> Option<&mut T> {
            self.inner.get_mut()
        }

        /// Sets the contents of this cell, returning `Err(value)` if already full.
        #[inline]
        pub fn set(&self, value: T) -> Result<(), T> {
            match self.try_insert(value) {
                Ok(_) => Ok(()),
                Err((_, value)) => Err(value),
            }
        }

        /// Sets the contents of this cell and returns a reference to the stored value.
        pub fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
            let mut value = Some(value);
            let res = self.get_or_init(|| {
                value
                    .take()
                    .unwrap_or_else(|| unreachable!("the initialiser runs at most once"))
            });
            match value {
                None => Ok(res),
                Some(value) => Err((res, value)),
            }
        }

        /// Gets the contents of the cell, initializing it with `f` if empty.
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> T,
        {
            match self.get_or_try_init(|| Ok::<T, Infallible>(f())) {
                Ok(val) => val,
                Err(never) => match never {},
            }
        }

        /// Gets the contents of the cell, initializing it with fallible `f` if empty.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<T, E>,
        {
            // Fast path: fully initialised, no lock taken. `OnceLock::get`
            // carries the acquire ordering that makes the value visible.
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let _guard = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let val = f()?;
            let _ = self.inner.set(val);
            Ok(self
                .get()
                .unwrap_or_else(|| unreachable!("just initialised under the gate")))
        }

        /// Takes the value out of the cell, leaving it empty.
        #[inline]
        pub fn take(&mut self) -> Option<T> {
            self.inner.take()
        }

        /// Consumes the cell, returning the wrapped value.
        #[inline]
        pub fn into_inner(self) -> Option<T> {
            self.inner.into_inner()
        }
    }

    /// Thread-safe value initialized on first access.
    /// If the initializer panics, the `Lazy` is poisoned and subsequent accesses panic.
    pub struct Lazy<T, F = fn() -> T> {
        cell: OnceCell<T>,
        init: Mutex<Option<F>>,
    }

    impl<T: fmt::Debug, F> fmt::Debug for Lazy<T, F> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Lazy")
                .field("cell", &self.cell)
                .field("init", &"..")
                .finish()
        }
    }

    impl<T, F> Lazy<T, F> {
        /// Creates a new lazy value with the given initializing function.
        #[inline]
        pub const fn new(f: F) -> Lazy<T, F> {
            Lazy {
                cell: OnceCell::new(),
                init: Mutex::new(Some(f)),
            }
        }

        /// Consumes this `Lazy`, returning `Ok(value)` if initialized or `Err(f)` if not.
        pub fn into_value(this: Lazy<T, F>) -> Result<T, F> {
            let cell = this.cell;
            let init = this.init;
            cell.into_inner().ok_or_else(|| {
                init.into_inner()
                    .unwrap_or_else(PoisonError::into_inner)
                    .unwrap_or_else(|| panic!("Lazy instance has previously been poisoned"))
            })
        }
    }

    impl<T, F: FnOnce() -> T> Lazy<T, F> {
        /// Forces evaluation and returns a shared reference to the result.
        pub fn force(this: &Lazy<T, F>) -> &T {
            this.cell.get_or_init(|| {
                let f = this
                    .init
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                match f {
                    Some(f) => f(),
                    None => panic!("Lazy instance has previously been poisoned"),
                }
            })
        }

        /// Forces evaluation and returns a mutable reference to the result.
        pub fn force_mut(this: &mut Lazy<T, F>) -> &mut T {
            if this.cell.get_mut().is_none() {
                let f = this
                    .init
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let value = match f {
                    Some(f) => f(),
                    None => panic!("Lazy instance has previously been poisoned"),
                };
                let _ = this.cell.set(value);
            }
            this.cell
                .get_mut()
                .unwrap_or_else(|| unreachable!("just initialised"))
        }

        /// Gets a shared reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get(this: &Lazy<T, F>) -> Option<&T> {
            this.cell.get()
        }

        /// Gets a mutable reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
            this.cell.get_mut()
        }
    }

    impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
        type Target = T;
        #[inline]
        fn deref(&self) -> &T {
            Lazy::force(self)
        }
    }

    impl<T, F: FnOnce() -> T> DerefMut for Lazy<T, F> {
        #[inline]
        fn deref_mut(&mut self) -> &mut T {
            Lazy::force_mut(self)
        }
    }

    impl<T: Default> Default for Lazy<T> {
        /// Creates a new lazy value using `Default` as the initialising function.
        #[inline]
        fn default() -> Lazy<T> {
            Lazy::new(T::default)
        }
    }
}

/// Thread-safe [`OnceBox`](race::OnceBox) compatibility module.
pub mod race {
    use core::fmt;
    use core::ptr;

    /// Thread-safe cell holding a heap-allocated `Box<T>` written at most once.
    pub struct OnceBox<T>(crate::sync::OnceCell<Box<T>>);

    impl<T> fmt::Debug for OnceBox<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let ptr = self.get().map_or(ptr::null(), |v| v as *const T);
            write!(f, "OnceBox({ptr:?})")
        }
    }

    impl<T> Default for OnceBox<T> {
        #[inline]
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: Clone> Clone for OnceBox<T> {
        fn clone(&self) -> Self {
            match self.get() {
                Some(value) => OnceBox::with_value(Box::new(value.clone())),
                None => OnceBox::new(),
            }
        }
    }

    impl<T> OnceBox<T> {
        /// Creates a new empty cell.
        #[inline]
        pub const fn new() -> Self {
            OnceBox(crate::sync::OnceCell::new())
        }

        /// Creates a new initialized cell with `value`.
        #[inline]
        pub fn with_value(value: Box<T>) -> Self {
            OnceBox(crate::sync::OnceCell::with_value(value))
        }

        /// Gets a shared reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.0.get().map(|b| &**b)
        }

        /// Sets the contents of this cell, returning `Err(value)` if already full.
        #[inline]
        pub fn set(&self, value: Box<T>) -> Result<(), Box<T>> {
            self.0.set(value)
        }

        /// Gets the contents of the cell, initializing it with `f` if empty.
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> Box<T>,
        {
            self.0.get_or_init(f)
        }

        /// Gets the contents of the cell, initializing it with fallible `f` if empty.
        #[inline]
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<Box<T>, E>,
        {
            self.0.get_or_try_init(f).map(|b| &**b)
        }
    }
}
