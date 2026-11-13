// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::Point;
type Hook = Box<dyn FnMut(Point) -> Option<()>>;
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}
pub(in crate::build_cache::registered) fn set(hook: impl FnMut(Point) -> Option<()> + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
pub(in crate::build_cache::registered) fn clear() {
    HOOK.with(|slot| *slot.borrow_mut() = None);
}
pub(super) fn at(point: Point) -> Option<()> {
    HOOK.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map_or(Some(()), |hook| hook(point))
    })
}
