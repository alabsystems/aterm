// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

type ReplyData = (u32, u32, i32, Option<&'static CStr>);

struct Fake {
    start_error: i32,
    private_error: bool,
    replies: RefCell<VecDeque<ReplyData>>,
    callback: Cell<Option<(Reply, *mut c_void)>>,
    closed: Cell<usize>,
}

impl Fake {
    fn new(replies: impl IntoIterator<Item = ReplyData>) -> Self {
        Self {
            start_error: 0,
            private_error: false,
            replies: RefCell::new(replies.into_iter().collect()),
            callback: Cell::new(None),
            closed: Cell::new(0),
        }
    }
}

impl Api for Fake {
    unsafe fn start(
        &self,
        _service: &CStr,
        _domain: &CStr,
        reply: Reply,
        context: *mut c_void,
    ) -> Result<Reference, i32> {
        if self.start_error != 0 {
            return Err(self.start_error);
        }
        self.callback.set(Some((reply, context)));
        Ok(std::ptr::NonNull::<u8>::dangling().as_ptr().cast())
    }

    unsafe fn keep_private(&self, _reference: Reference) -> io::Result<()> {
        if self.private_error {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else {
            Ok(())
        }
    }

    unsafe fn process(&self, reference: Reference) -> i32 {
        let Some((flags, interface, error, name)) = self.replies.borrow_mut().pop_front() else {
            return -65537;
        };
        let (reply, context) = self.callback.get().unwrap();
        // SAFETY: browse_with keeps this context live until close. Each success
        // supplies a static C string; errors may leave every other field NULL.
        unsafe {
            reply(
                reference,
                flags,
                interface,
                error,
                name.map_or(std::ptr::null(), CStr::as_ptr),
                std::ptr::null(),
                std::ptr::null(),
                context,
            );
        }
        0
    }

    unsafe fn close(&self, _reference: Reference) {
        self.closed.set(self.closed.get() + 1);
        self.callback.set(None);
    }
}

#[test]
fn native_entry_is_inert_in_a_unit_test() {
    let error = browse(c"_alab._tcp", c"local.", |_| panic!("no native callback")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn the_loopback_guard_refuses_the_native_browser_before_it_can_start() {
    let error = crate::loopback::forced(true, || {
        browse(c"_alab._tcp", c"local.", |_| panic!("no native callback"))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn added_flag_is_a_bit_and_names_remain_unescaped_until_the_callback_ends() {
    let api = Fake::new([(ADD | 1, 4, 0, Some(c"Desk A")), (0, 9, 0, Some(c"Desk A"))]);
    let mut heard = Vec::new();
    browse_with(&api, c"_alab._tcp", c"local.", |event| {
        heard.push((event.added, event.interface, event.name.to_owned()));
        if heard.len() == 2 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .unwrap();
    assert_eq!(
        heard,
        [
            (true, 4, c"Desk A".to_owned()),
            (false, 9, c"Desk A".to_owned())
        ]
    );
    assert_eq!(api.closed.get(), 1);
    assert!(api.callback.get().is_none());
}

#[test]
fn startup_and_descriptor_failures_release_only_a_successfully_started_reference() {
    let mut api = Fake::new([]);
    api.start_error = -65537;
    assert!(
        browse_with(
            &api,
            c"_alab._tcp",
            c"local.",
            |_| ControlFlow::Continue(())
        )
        .is_err()
    );
    assert_eq!(api.closed.get(), 0);
    api.start_error = 0;
    api.private_error = true;
    assert_eq!(
        browse_with(
            &api,
            c"_alab._tcp",
            c"local.",
            |_| ControlFlow::Continue(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(api.closed.get(), 1);
}

#[test]
fn asynchronous_errors_never_read_undefined_callback_fields() {
    let api = Fake::new([(0, 0, -65537, None)]);
    assert!(
        browse_with(&api, c"_alab._tcp", c"local.", |_| panic!(
            "error is not an event"
        ))
        .is_err()
    );
    assert_eq!(api.closed.get(), 1);
}

#[test]
fn transport_failure_and_callback_panic_both_close_the_reference() {
    let api = Fake::new([]);
    assert!(
        browse_with(
            &api,
            c"_alab._tcp",
            c"local.",
            |_| ControlFlow::Continue(())
        )
        .is_err()
    );
    assert_eq!(api.closed.get(), 1);
    let api = Fake::new([(ADD, 4, 0, Some(c"desk"))]);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = browse_with(&api, c"_alab._tcp", c"local.", |_| panic!("callback"));
    }));
    assert!(panic.is_err());
    assert_eq!(api.closed.get(), 1);
    assert!(api.callback.get().is_none());
}
