// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use super::{Shelf, pick_icon_image, render_off_thread};

#[test]
fn the_icon_is_made_on_a_worker_and_never_on_the_asking_thread() {
    use std::sync::mpsc;
    let asker = std::thread::current().id();
    let (rendered_tx, rendered) = mpsc::channel();
    let (delivered_tx, delivered) = mpsc::channel();
    let (go_tx, go) = mpsc::channel::<()>();
    let started = render_off_thread(
        crate::qos::Role::Responsive,
        move || {
            let _ = rendered_tx.send(std::thread::current().id());
            // Held until the asker has its answer back: the ask must not wait for the icon.
            go.recv_timeout(Duration::from_secs(60)).ok()?;
            Some(7_u8)
        },
        move |icon| {
            let _ = delivered_tx.send((std::thread::current().id(), icon));
        },
    );
    assert!(started, "the worker started");
    let worker = rendered
        .recv_timeout(Duration::from_secs(60))
        .expect("the icon source was asked");
    assert_ne!(worker, asker, "the icon source ran on the asking thread");
    go_tx.send(()).expect("the worker waits");
    let (on, icon) = delivered
        .recv_timeout(Duration::from_secs(60))
        .expect("the icon was delivered");
    assert_eq!((on, icon), (worker, Some(7)), "delivered from the worker");
}

#[test]
fn the_icon_file_image_is_the_narrowest_wide_enough_else_the_widest() {
    // An `.icns` lists 16, 32, 64, 128, 256, 512 and 1024 pixel images, @1x and @2x pairs
    // repeating widths.
    let icns = [16, 32, 32, 64, 128, 256, 256, 512, 512, 1024];
    assert_eq!(pick_icon_image(&icns, 128), Some(4), "exact");
    assert_eq!(pick_icon_image(&icns, 192), Some(5), "next wider");
    assert_eq!(pick_icon_image(&icns, 2048), Some(9), "the widest there is");
    assert_eq!(
        pick_icon_image(&[0, 32, 0], 64),
        Some(1),
        "unknown widths skipped"
    );
    assert_eq!(pick_icon_image(&[0, 0], 64), None);
    assert_eq!(pick_icon_image(&[], 64), None);
}

#[test]
fn an_alert_takes_its_looks_icon_else_another_looks_and_each_look_is_made_once() {
    let mut shelf = Shelf::new();
    assert_eq!(shelf.pick("NSAppearanceNameAqua"), None, "nothing made yet");
    assert!(
        shelf.wants("NSAppearanceNameAqua"),
        "the first ask starts a worker"
    );
    assert!(!shelf.wants("NSAppearanceNameAqua"), "one worker per look");
    shelf.put("NSAppearanceNameAqua", Some(1_u8));
    assert!(!shelf.wants("NSAppearanceNameAqua"), "made: no worker");
    assert_eq!(shelf.pick("NSAppearanceNameAqua"), Some(&1));
    assert_eq!(
        shelf.pick("NSAppearanceNameDarkAqua"),
        Some(&1),
        "another look's icon beats an IconServices draw on the main thread"
    );
    assert!(
        shelf.wants("NSAppearanceNameDarkAqua"),
        "the new look is made"
    );
    shelf.put("NSAppearanceNameDarkAqua", Some(2));
    assert_eq!(shelf.pick("NSAppearanceNameDarkAqua"), Some(&2), "its own");
    assert_eq!(shelf.pick("NSAppearanceNameAqua"), Some(&1), "its own");
    assert!(shelf.wants("Other"));
    shelf.put("Other", None);
    assert!(
        shelf.wants("Other"),
        "a worker that made nothing may be asked again"
    );
    shelf.forget("Other");
    assert!(
        shelf.wants("Other"),
        "a worker that never started may be asked again"
    );
}

/// The bundle's icon file drawn into a bitmap of the icon's own is plain decoded pixels at the
/// icon's size: an image straight from ImageIO is not (negative control: return `icns_image`'s
/// image undrawn and the pixel-layout check fails at 256, the size check too at 100).
#[cfg(target_os = "macos")]
#[test]
fn the_icon_crosses_threads_as_decoded_pixels_of_its_own_size() {
    use super::PIXELS;
    use super::live::{BITMAP_INFO, decoded, icns_image};

    use aterm_cg::{CGImageGetBitmapInfo, CGImageGetHeight, CGImageGetWidth};

    let icns = aterm_tempfile::manifest_dir().join("../../apps/aterm-mac/aterm.icns");
    // A size no icns entry has, so only a draw makes it.
    for want in [PIXELS, 100] {
        // SAFETY: a test thread; the images are live while read.
        unsafe {
            let raw = icns_image(&icns, want).expect("the checkout's aterm.icns reads");
            let made = decoded(raw.as_ptr(), want).expect("drawn into a bitmap");
            let image = made.as_ptr();
            assert_eq!(
                (CGImageGetWidth(image), CGImageGetHeight(image)),
                (want, want),
                "drawn at the size asked"
            );
            assert_eq!(
                CGImageGetBitmapInfo(image),
                BITMAP_INFO,
                "the bitmap context's own pixels"
            );
        }
    }
}
