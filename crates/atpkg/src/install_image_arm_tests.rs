// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::Path;

use super::{Attached, StageError};

/// What attaches an image for a stage: `(image, dest)` to the attached image.
pub(super) type Arm = Box<dyn Fn(&Path, &Path) -> Result<Box<dyn Attached>, StageError>>;

thread_local! {
    static ARM: RefCell<Option<Arm>> = const { RefCell::new(None) };
}

/// Run `f` with `arm` attaching this thread's images; none again after, unwinding included.
pub(super) fn with<R>(arm: Arm, f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ARM.with(|a| *a.borrow_mut() = None);
        }
    }
    ARM.with(|a| *a.borrow_mut() = Some(arm));
    let _reset = Reset;
    f()
}

/// Attach `image` for the stage at `dest` with this thread's arm.
pub(super) fn attach(image: &Path, dest: &Path) -> Result<Box<dyn Attached>, StageError> {
    ARM.with(|a| match a.borrow().as_ref() {
        Some(arm) => arm(image, dest),
        None => Err(StageError::Payload(String::from(
            "this test thread attaches no disk image: a default test run never runs hdiutil",
        ))),
    })
}
