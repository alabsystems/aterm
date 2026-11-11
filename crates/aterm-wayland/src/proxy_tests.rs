// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{ErrorKind, Read};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;

use super::*;
use crate::iface::{NULLS, TypeRef};

/// A socket pair: the far end's descriptor for an event, and the near end.
fn pair() -> (c_int, UnixStream) {
    let (near, far) = UnixStream::pair().expect("socketpair");
    near.set_nonblocking(true).expect("nonblocking");
    (far.into_raw_fd(), near)
}

/// Whether the far end of `near` is closed.
fn far_closed(near: &mut UnixStream) -> bool {
    match near.read(&mut [0u8; 1]) {
        Ok(0) => true,
        Err(e) if e.kind() == ErrorKind::WouldBlock => false,
        other => panic!("read: {other:?}"),
    }
}

static KEYMAP: Message = Message::new(c"keymap", c"uhu", &NULLS);
static WIDE_TYPES: [TypeRef; 17] = [
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
];
static WIDE: Message = Message::new(c"wide", c"uuuuuuuuuuuuuuuuh", &WIDE_TYPES);

#[test]
fn an_event_no_handler_takes_has_its_descriptor_closed() {
    let (fd, mut near) = pair();
    let mut args = [
        WlArgument { u: 1 },
        WlArgument { h: fd },
        WlArgument { u: 2 },
    ];
    let target = NonNull::<c_void>::dangling().as_ptr();
    // SAFETY: The event's arguments match its signature; no handler.
    unsafe {
        dispatch(
            std::ptr::null(),
            target,
            0,
            &raw const KEYMAP,
            args.as_mut_ptr(),
        )
    };
    assert!(far_closed(&mut near), "the descriptor stayed open");
}

#[test]
fn an_event_past_max_args_has_its_descriptor_closed() {
    let (fd, mut near) = pair();
    let mut args = [WlArgument { u: 0 }; MAX_ARGS + 1];
    args[MAX_ARGS] = WlArgument { h: fd };
    let mut handler: Box<Handler> = Box::new(|_, _, _| panic!("past MAX_ARGS, never called"));
    let data = std::ptr::from_mut(&mut handler).cast_const().cast();
    let target = NonNull::<c_void>::dangling().as_ptr();
    // SAFETY: As above, with a handler that is not reached.
    unsafe { dispatch(data, target, 0, &raw const WIDE, args.as_mut_ptr()) };
    assert!(far_closed(&mut near), "the descriptor stayed open");
}

/// The negative control: a handler receives the descriptor, and owns it.
#[test]
fn a_handled_event_leaves_its_descriptor_to_the_handler() {
    let (fd, mut near) = pair();
    let mut args = [
        WlArgument { u: 1 },
        WlArgument { h: fd },
        WlArgument { u: 2 },
    ];
    let got = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::clone(&got);
    let mut handler: Box<Handler> = Box::new(move |_, _, args| {
        if let [_, EventArg::Fd(fd), _] = args {
            *seen.lock().expect("lock") = Some(*fd);
        }
    });
    let data = std::ptr::from_mut(&mut handler).cast_const().cast();
    let target = NonNull::<c_void>::dangling().as_ptr();
    // SAFETY: As above.
    unsafe { dispatch(data, target, 0, &raw const KEYMAP, args.as_mut_ptr()) };
    assert_eq!(*got.lock().expect("lock"), Some(fd));
    assert!(
        !far_closed(&mut near),
        "the dispatcher closed a handled descriptor"
    );
    // SAFETY: The descriptor the handler was given, closed by its owner.
    drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
    assert!(far_closed(&mut near));
}

#[test]
fn negative_fd_and_null_args_are_ignored_without_invalid_owned_fd() {
    let mut args = [
        WlArgument { u: 1 },
        WlArgument { h: -1 },
        WlArgument { u: 2 },
    ];
    let target = NonNull::<c_void>::dangling().as_ptr();
    // SAFETY: Malformed negative fd and null argument vector are ignored safely.
    unsafe {
        dispatch(
            std::ptr::null(),
            target,
            0,
            &raw const KEYMAP,
            args.as_mut_ptr(),
        );
        dispatch(
            std::ptr::null(),
            target,
            0,
            &raw const KEYMAP,
            std::ptr::null_mut(),
        );
    }
}
