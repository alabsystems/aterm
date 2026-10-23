// Copyright 2026 Andrew Yates
// Copyright (c) 2018-2025 David Tolnay
// SPDX-License-Identifier: Apache-2.0
//
// Derived from inventory 0.3.24 (src/lib.rs), offered under MIT OR Apache-2.0
// and taken here under Apache-2.0. Modified by the aterm project in 2026: the
// nodes are typed (no `dyn` erasure and no cast back on the way out, with a
// `T: Sync` bound on the types in place of `ErasedNode: Sync`), the link
// pointers are atomics, a node links at most once, and a target with no
// constructor section is a compile error rather than an empty registry.

//! Link-time collection: the distributed slice behind [`crate::xref`] and `aterm_spec::xref`.

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// A type whose values [`submit!`] can collect.
pub trait Collect: Sync + Sized + 'static {
    /// This type's one list.
    #[doc(hidden)]
    fn registry() -> &'static Registry<Self>;
}

/// The head of one type's list.
pub struct Registry<T: Sync + 'static> {
    head: AtomicPtr<Node<T>>,
}

/// One submitted value and its link.
pub struct Node<T: Sync + 'static> {
    value: T,
    next: AtomicPtr<Node<T>>,
    linked: AtomicBool,
}

impl<T: Sync> Node<T> {
    #[doc(hidden)]
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            value,
            next: AtomicPtr::new(ptr::null_mut()),
            linked: AtomicBool::new(false),
        }
    }
}

impl<T: Sync> Registry<T> {
    #[doc(hidden)]
    #[allow(
        clippy::new_without_default,
        reason = "built only in a `static` by `collect!`, where `Default` cannot run"
    )]
    #[inline]
    pub const fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Push `node` onto the front of the list.
    pub fn submit(&'static self, node: &'static Node<T>) {
        if node.linked.swap(true, Ordering::Relaxed) {
            return;
        }
        let new = ptr::from_ref(node).cast_mut();
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            node.next.store(head, Ordering::Relaxed);
            match self
                .head
                .compare_exchange_weak(head, new, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(now) => head = now,
            }
        }
    }

    /// Every value submitted so far, newest first.
    #[inline]
    pub fn iter(&'static self) -> Iter<T> {
        Iter {
            // SAFETY: null or a `&'static Node<T>` (the `Registry` invariant).
            next: unsafe { self.head.load(Ordering::Acquire).as_ref() },
        }
    }
}

/// Every value of `T` the binary submitted, newest first.
#[inline]
pub fn iter<T: Collect>() -> Iter<T> {
    T::registry().iter()
}

/// The walk [`iter`] returns.
pub struct Iter<T: Sync + 'static> {
    next: Option<&'static Node<T>>,
}

impl<T: Sync> Iterator for Iter<T> {
    type Item = &'static T;

    #[inline]
    fn next(&mut self) -> Option<&'static T> {
        let node = self.next?;
        // SAFETY: null or a `&'static Node<T>` (the `Registry` invariant).
        self.next = unsafe { node.next.load(Ordering::Acquire).as_ref() };
        Some(&node.value)
    }
}

/// `collect!(T)`: give `T` its [`Registry`], so [`submit!`] and [`iter`] take it.
#[doc(hidden)]
#[macro_export]
macro_rules! __registry_collect {
    ($ty:ty) => {
        impl $crate::registry::Collect for $ty {
            #[inline]
            fn registry() -> &'static $crate::registry::Registry<$ty> {
                static REGISTRY: $crate::registry::Registry<$ty> =
                    $crate::registry::Registry::new();
                &REGISTRY
            }
        }
    };
}

/// `submit!(T => value)`: collect `value` into `T`'s list when the image loads.
#[doc(hidden)]
#[macro_export]
macro_rules! __registry_submit {
    ($ty:ty => $value:expr) => {
        const _: () = {
            static NODE: $crate::registry::Node<$ty> = $crate::registry::Node::new($value);

            extern "C" fn submit_at_load() {
                <$ty as $crate::registry::Collect>::registry().submit(&NODE);
            }

            #[cfg(not(any(
                target_vendor = "apple",
                target_os = "linux",
                target_os = "android",
                windows
            )))]
            ::core::compile_error!(
                "aterm_spec_anchor::registry has no load-time constructor section for this \
                 target, so nothing submitted here would ever be collected"
            );

            #[used]
            #[cfg_attr(
                target_vendor = "apple",
                unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
            )]
            #[cfg_attr(
                any(target_os = "linux", target_os = "android"),
                unsafe(link_section = ".init_array")
            )]
            #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
            static SUBMIT_AT_LOAD: extern "C" fn() = submit_at_load;
        };
    };
}

#[doc(inline)]
pub use crate::__registry_collect as collect;
#[doc(inline)]
pub use crate::__registry_submit as submit;

#[cfg(test)]
mod tests {
    use super::{Node, Registry};

    struct Seen(u32);

    crate::registry::collect!(Seen);
    crate::registry::submit!(Seen => Seen(1));
    crate::registry::submit!(Seen => Seen(2));

    #[test]
    fn the_load_time_constructors_collected_both_submits() {
        let mut seen: Vec<u32> = super::iter::<Seen>().map(|s| s.0).collect();
        seen.sort_unstable();
        assert_eq!(seen, [1, 2]);
    }

    #[test]
    fn a_node_submitted_twice_is_linked_once() {
        static LIST: Registry<u8> = Registry::new();
        static ONE: Node<u8> = Node::new(1);
        static TWO: Node<u8> = Node::new(2);
        LIST.submit(&ONE);
        LIST.submit(&TWO);
        LIST.submit(&ONE);
        assert_eq!(LIST.iter().copied().collect::<Vec<_>>(), [2, 1]);
    }
}
