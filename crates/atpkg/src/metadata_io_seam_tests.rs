// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
type Callback = Box<dyn Fn(&Path) -> io::Result<()>>;
thread_local! {
    static BEFORE_COMMIT: std::cell::RefCell<Option<Callback>> = const { std::cell::RefCell::new(None) };
}
pub(crate) struct Guard(Option<Callback>);
impl Drop for Guard {
    fn drop(&mut self) {
        BEFORE_COMMIT.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}
pub(crate) fn before_publish(callback: impl Fn(&Path) -> io::Result<()> + 'static) -> Guard {
    Guard(BEFORE_COMMIT.with(|slot| slot.replace(Some(Box::new(callback)))))
}
pub(super) fn before_commit(path: &Path) -> io::Result<()> {
    BEFORE_COMMIT.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(Ok(()), |callback| callback(path))
    })
}
