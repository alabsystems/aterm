// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Refused;
use std::{cell::Cell, io, rc::Rc};

struct Original {
    identity: Rc<()>,
    drops: Rc<Cell<usize>>,
}

impl Drop for Original {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn refusal_keeps_the_same_error_and_original_until_recovered_or_dropped() {
    let identity = Rc::new(());
    let drops = Rc::new(Cell::new(0));
    let original = || Original {
        identity: Rc::clone(&identity),
        drops: Rc::clone(&drops),
    };
    let refused = Refused::new(
        io::Error::new(io::ErrorKind::TimedOut, "original transition error"),
        original(),
    );
    assert_eq!(refused.error().kind(), io::ErrorKind::TimedOut);
    assert_eq!(drops.get(), 0);
    let (error, retained) = refused.into_parts();
    assert_eq!(error.to_string(), "original transition error");
    assert!(Rc::ptr_eq(&identity, &retained.identity));
    assert_eq!(drops.get(), 0);
    drop(retained);
    assert_eq!(drops.get(), 1);

    drop(Refused::new(io::ErrorKind::InvalidInput.into(), original()));
    assert_eq!(drops.get(), 2);
}
