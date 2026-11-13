// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;

type Act = Box<dyn FnOnce(&str)>;

thread_local! {
    static ARMED: RefCell<Option<(&'static str, Act)>> = const { RefCell::new(None) };
}

/// Run `f` with `act` armed for the window `name` on this thread. It fires once, with
/// the path the window names. Panics if `f` never reached the window, so a test cannot
/// pass without its act having run. Disarmed after, unwinding included.
pub(crate) fn with<R>(
    name: &'static str,
    act: impl FnOnce(&str) + 'static,
    f: impl FnOnce() -> R,
) -> R {
    struct Disarm;
    impl Drop for Disarm {
        fn drop(&mut self) {
            ARMED.with(|armed| *armed.borrow_mut() = None);
        }
    }
    ARMED.with(|armed| *armed.borrow_mut() = Some((name, Box::new(act))));
    let disarm = Disarm;
    let out = f();
    let unreached = ARMED.with(|armed| armed.borrow().is_some());
    drop(disarm);
    assert!(!unreached, "the window `{name}` was never reached");
    out
}

/// The window `name`, at `path`: run the act armed for it, if any.
pub(crate) fn reached(name: &'static str, path: &str) {
    let act = ARMED.with(|armed| {
        let mut armed = armed.borrow_mut();
        match armed.as_ref() {
            Some((window, _)) if *window == name => armed.take().map(|(_, act)| act),
            _ => None,
        }
    });
    if let Some(act) = act {
        act(path);
    }
}
