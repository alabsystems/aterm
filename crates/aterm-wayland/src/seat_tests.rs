// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Write;

use super::*;

fn file_with(bytes: &[u8]) -> (aterm_tempfile::TempDir, OwnedFd) {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let path = dir.path().join("keymap");
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(bytes))
        .expect("the keymap file");
    let fd = OwnedFd::from(File::open(&path).expect("open"));
    (dir, fd)
}

#[test]
fn a_keymap_is_read_to_its_nul() {
    let body = b"xkb_keymap { };\n\0";
    let (_dir, fd) = file_with(body);
    let size = u32::try_from(body.len()).unwrap();
    let keymap = read_keymap(wl_keyboard::keymap_format::XKB_V1, fd, size).unwrap();
    assert_eq!(keymap.text, b"xkb_keymap { };\n");
}

#[test]
fn a_keymap_shorter_than_its_size_is_an_error_not_a_guess() {
    let (_dir, fd) = file_with(b"xkb_keymap");
    assert_eq!(
        read_keymap(wl_keyboard::keymap_format::XKB_V1, fd, 64),
        Err(KeymapError::Read(std::io::ErrorKind::UnexpectedEof))
    );
}

#[test]
fn a_keymap_past_the_limit_is_refused_unread() {
    let (_dir, fd) = file_with(b"x");
    assert_eq!(
        read_keymap(wl_keyboard::keymap_format::XKB_V1, fd, MAX_KEYMAP + 1),
        Err(KeymapError::TooLarge(MAX_KEYMAP + 1))
    );
}

#[test]
fn no_keymap_reads_nothing() {
    let (_dir, fd) = file_with(b"ignored");
    let keymap = read_keymap(wl_keyboard::keymap_format::NO_KEYMAP, fd, 7).unwrap();
    assert!(keymap.text.is_empty());
}

#[test]
fn enter_keys_are_native_words() {
    let mut bytes = Vec::new();
    for k in [30u32, 48] {
        bytes.extend(k.to_ne_bytes());
    }
    assert_eq!(words(&bytes), [30, 48]);
}

#[test]
fn latest_serial_tracks_arrival_order_across_u32_wraparound() {
    let mut frames = Frames::new(5);
    let mut latest = None;
    let mut focus = None;

    record_pointer(
        &mut frames,
        &mut latest,
        Raw::Enter {
            serial: u32::MAX,
            surface: None,
            x: 0.0,
            y: 0.0,
        },
    );
    assert_eq!(latest, Some(u32::MAX));

    record_keyboard(
        &mut latest,
        &mut focus,
        &KeyboardEvent::Key {
            serial: 0,
            time: 10,
            key: 30,
            state: 1,
        },
    );
    assert_eq!(latest, Some(0));

    record_pointer(
        &mut frames,
        &mut latest,
        Raw::Button {
            serial: 1,
            time: 20,
            button: 272,
            state: 1,
        },
    );
    assert_eq!(latest, Some(1));
}

#[test]
fn keymap_with_negative_fd_is_rejected_without_owned_fd_ub() {
    let got = keyboard_input(
        wl_keyboard::evt::KEYMAP,
        &[EventArg::Uint(1), EventArg::Fd(-1), EventArg::Uint(16)],
    );
    assert!(got.is_none());
}
