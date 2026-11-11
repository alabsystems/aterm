// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_rust_panic_passes_through_the_wrapper_to_catch_unwind() {
    use std::cell::Cell;
    let dropped = Cell::new(false);
    struct Tell<'a>(&'a Cell<bool>);
    impl Drop for Tell<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let handler_ran = Cell::new(false);
    let outer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        objc_try(
            || {
                let _t = Tell(&dropped);
                if dropped.get() {
                    return 1;
                }
                panic!("a Rust panic inside the body");
            },
            |_| handler_ran.set(true),
        )
    }));
    assert!(outer.is_err(), "the panic reached the OUTER catch_unwind");
    assert!(dropped.get(), "the body's local was dropped by the unwind");
    assert!(
        !handler_ran.get(),
        "@catch(id) must not swallow a Rust panic"
    );
}

/// The handler is released on every way out of the wrapper, including the Rust-panic pass-through where it never ran.
#[test]
fn the_unrun_handler_is_dropped_on_both_the_ok_and_the_panic_path() {
    use std::rc::Rc;
    let token = Rc::new(());
    let handler_token = Rc::clone(&token);
    let r = objc_try(
        || 1,
        move |_| {
            let _ = &handler_token;
        },
    );
    assert_eq!(r, Ok(1));
    assert_eq!(
        Rc::strong_count(&token),
        1,
        "the handler was dropped on the Ok path"
    );

    let handler_token = Rc::clone(&token);
    let outer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        objc_try(
            || -> i32 { panic!("a Rust panic inside the body") },
            move |_| {
                let _ = &handler_token;
            },
        )
    }));
    assert!(outer.is_err());
    assert_eq!(
        Rc::strong_count(&token),
        1,
        "the handler that never ran was dropped on the panic path"
    );
}
