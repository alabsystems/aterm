// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_display_names_its_socket() {
    assert_eq!(
        socket_path(":91"),
        Some(PathBuf::from("/tmp/.X11-unix/X91"))
    );
    assert_eq!(
        socket_path(":92.0"),
        Some(PathBuf::from("/tmp/.X11-unix/X92"))
    );
    assert_eq!(socket_path("host:0"), None);
    assert_eq!(socket_path(":"), None);
}

/// Requests as the X11R7.7 protocol encodes them (little-endian).
#[test]
fn requests_are_encoded_as_the_protocol_lays_them_out() {
    assert_eq!(
        intern_atom("XdndAware"),
        [
            16, 0, 5, 0, 9, 0, 0, 0, b'X', b'd', b'n', b'd', b'A', b'w', b'a', b'r', b'e', 0, 0, 0
        ]
    );
    let cw = create_window(0x0020_0001, 0x0000_0505);
    assert_eq!(cw.len(), 32);
    assert_eq!(&cw[..4], [1, 0, 8, 0]);
    assert_eq!(u32_at(&cw, 4), Some(0x0020_0001));
    assert_eq!(u32_at(&cw, 8), Some(0x0505));
    assert_eq!(u16_at(&cw, 16), Some(1), "width");
    assert_eq!(u16_at(&cw, 22), Some(2), "InputOnly");
    let cp = change_property(7, 8, 9, b"file:///a\r\n");
    assert_eq!(&cp[..4], [18, 0, 9, 0]);
    assert_eq!(cp[16], 8, "format 8");
    assert_eq!(u32_at(&cp, 20), Some(11), "length in bytes");
    assert_eq!(&cp[24..35], b"file:///a\r\n");
    assert_eq!(cp.len(), 36, "padded to a word");
    let se = send_event(3, &client_message(3, 4, [1, 2, 3, 4, 5]));
    assert_eq!(&se[..4], [25, 0, 11, 0]);
    assert_eq!(se.len(), 44);
    assert_eq!(se[12], 33, "a ClientMessage");
    assert_eq!(se[13], 32, "format 32");
    assert_eq!(u32_at(&se, 12 + 12), Some(1));
    assert_eq!(u32_at(&se, 12 + 28), Some(5));
    assert_eq!(get_property(1, 2).len(), 24);
    assert_eq!(set_selection_owner(1, 2)[..4], [22, 0, 4, 0]);
    assert_eq!(get_geometry(1)[..4], [14, 0, 2, 0]);
    let tc = translate_coordinates(1, 2, -3, 4);
    assert_eq!(&tc[..4], [40, 0, 4, 0]);
    assert_eq!(i16_at(&tc, 12), Some(-3));
}

#[test]
fn the_setup_reply_names_the_root_and_the_ids() {
    let mut r = vec![1, 0, 11, 0, 0, 0, 0, 0];
    r.extend_from_slice(&words(&[0, 0x0020_0000, 0x001f_ffff, 0]));
    r.extend_from_slice(&5u16.to_le_bytes()); // vendor length
    r.extend_from_slice(&[0, 0, 1, 2, 0, 0, 0, 0, 8, 255, 0, 0, 0, 0]);
    r.extend_from_slice(b"Xorg\0\0\0\0"); // vendor, padded to 8
    r.extend_from_slice(&[0; 16]); // two formats
    r.extend_from_slice(&0x0000_0505u32.to_le_bytes());
    assert_eq!(
        parse_setup(&r),
        Ok(Setup {
            id_base: 0x0020_0000,
            id_mask: 0x001f_ffff,
            root: 0x0505,
            min_keycode: 8,
            max_keycode: 255,
        })
    );
    let mut refused = vec![0, 6, 11, 0, 0, 0, 2, 0];
    refused.extend_from_slice(b"no way\0\0");
    assert!(
        parse_setup(&refused)
            .expect_err("refused")
            .ends_with("no way")
    );
    assert!(parse_setup(&r[..30]).is_err(), "short");
    assert!(parse_setup(&[]).is_err());
}

#[test]
fn events_decode_by_their_code() {
    let cm = client_message(9, 10, [11, 1, 0, 0, 12]);
    assert_eq!(
        decode(&cm),
        Event::ClientMessage {
            window: 9,
            type_: 10,
            data: [11, 1, 0, 0, 12]
        }
    );
    let mut sent = selection_notify(1, 2, 3, 4, 5);
    sent[0] |= 0x80;
    assert_eq!(
        decode(&sent),
        Event::Other(31),
        "the sent-event bit is cleared"
    );
    let mut req = [0u8; 32];
    req[0] = 30;
    for (i, v) in [7u32, 8, 9, 10, 11, 12].iter().enumerate() {
        req[4 + 4 * i..8 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    assert_eq!(
        decode(&req),
        Event::SelectionRequest {
            time: 7,
            requestor: 9,
            selection: 10,
            target: 11,
            property: 12
        }
    );
    let mut err = [0u8; 32];
    err[1] = 3;
    err[10] = 25;
    assert_eq!(decode(&err), Event::Error { code: 3, major: 25 });
}
