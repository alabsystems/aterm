// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_read_stops_at_its_limit_and_says_so() {
    assert_eq!(read_limited(&b"abc"[..], 3).unwrap(), b"abc");
    let err = read_limited(&b"abcd"[..], 3).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_detached_write_reaches_the_reader_and_closes() {
    let (mut reader, writer) = io::pipe().unwrap();
    write_detached(OwnedFd::from(writer), b"LANE-42".to_vec()).unwrap();
    let mut got = Vec::new();
    reader.read_to_end(&mut got).unwrap();
    assert_eq!(got, b"LANE-42", "and the end of the pipe came");
}

fn empty_book() -> Mutex<DeviceBook> {
    Mutex::new(DeviceBook {
        offers: Vec::new(),
        selection: None,
        drag: None,
    })
}

#[test]
fn a_drag_s_events_come_back_for_the_sink_in_the_order_heard() {
    let book = empty_book();
    let motion = [
        EventArg::Uint(7),
        EventArg::Fixed(crate::proxy::Fixed::from_f64(1.5)),
        EventArg::Fixed(crate::proxy::Fixed::from_f64(-2.0)),
    ];
    let heard: Vec<Option<DragEvent>> = [
        (wl_data_device::evt::MOTION, &motion[..]),
        (wl_data_device::evt::DROP, &[][..]),
        (wl_data_device::evt::LEAVE, &[][..]),
    ]
    .into_iter()
    .map(|(op, args)| on_device(&book, Kind::Clipboard, op, args))
    .collect();
    assert_eq!(
        heard,
        [
            Some(DragEvent::Motion {
                time: 7,
                x: 1.5,
                y: -2.0
            }),
            Some(DragEvent::Drop),
            Some(DragEvent::Leave),
        ]
    );
}

#[test]
fn a_selection_and_a_primary_device_make_no_drag_event() {
    let book = empty_book();
    let none = [EventArg::Object(None)];
    assert_eq!(
        on_device(
            &book,
            Kind::Clipboard,
            wl_data_device::evt::SELECTION,
            &none
        ),
        None
    );
    assert_eq!(
        on_device(&book, Kind::Primary, wl_data_device::evt::DROP, &[]),
        None
    );
    assert_eq!(
        on_device(&book, Kind::Clipboard, wl_data_device::evt::DROP, &[]),
        Some(DragEvent::Drop),
        "the same opcode on the clipboard device is a drop"
    );
}

#[test]
fn a_mime_type_with_a_nul_is_refused() {
    assert!(mime("text/plain;charset=utf-8").is_some());
    assert!(mime("a\0b").is_none());
}

#[test]
fn source_send_with_negative_fd_is_rejected_without_owned_fd_ub() {
    let got = source_request(
        Kind::Clipboard,
        wl_data_source::evt::SEND,
        &[EventArg::Str(Some(c"text/plain")), EventArg::Fd(-1)],
    );
    assert!(got.is_none());
}
