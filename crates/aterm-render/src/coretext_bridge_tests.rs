// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Core Text fonts this crate makes before any AppKit use leave AppKit's fonts working.
//!
//! An update successor's font workers made the process's first Core Text fonts before its
//! window code loaded AppKit; the window's first control then asked NSFont for a font and
//! the process aborted on `initWithTypefaceInfo:key:renderingMode:`. The test repeats that
//! order in a fresh process, where nothing has loaded AppKit yet.

use crate::macos_coretext;
use aterm_objc::{Bool, ClassPtr, Id, Sel};
use std::time::Duration;

const CHILD: &str = "ATERM_TEST_CORETEXT_BRIDGE_CHILD";
const NAME: &str = "coretext_bridge_tests::fonts_made_before_appkit_leave_nsfont_working";
const REACHED: &str = "AppKit's fonts answered: ";

#[test]
fn fonts_made_before_appkit_leave_nsfont_working() {
    if std::env::var_os(CHILD).is_some() {
        fonts_first_then_appkit();
        return;
    }
    let mut command =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary's path"));
    command
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, "1");
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60))
        .expect("the fresh process finishes before the hang detector");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the fresh process failed: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(REACHED),
        "the fresh process must show it reached AppKit's font calls: {stdout}"
    );
}

/// The successor's order: this crate's fonts first, then AppKit and NSFont.
fn fonts_first_then_appkit() {
    assert!(
        aterm_objc::class(c"NSFont").is_null(),
        "AppKit is already loaded, so this process cannot make a font before it"
    );
    assert!(
        macos_coretext::font_path_for_char('✓').is_some(),
        "Core Text names a face for ✓"
    );
    let menlo =
        crate::font_file::admit_font_file(std::path::Path::new("/System/Library/Fonts/Menlo.ttc"))
            .expect("macOS ships Menlo.ttc");
    let font = macos_coretext::CtFont::new(menlo, 0, 12.0, &[]).expect("Menlo's first face");

    aterm_macos_ui::appkit::load();
    let answered = aterm_objc::autoreleasepool(|_| {
        aterm_objc::exception::contain("fonts made before AppKit", || {
            // SAFETY: each signature matches its selector's: isKindOfClass: takes a class and
            // returns BOOL, systemFontOfSize: takes a CGFloat and returns an autoreleased
            // NSFont, used only inside this pool. The Core Text font is live: `font` owns it.
            unsafe {
                let is_kind: unsafe extern "C-unwind" fn(Id, Sel, ClassPtr) -> Bool =
                    aterm_objc::checked_msg();
                let system: unsafe extern "C-unwind" fn(Id, Sel, f64) -> Id =
                    aterm_objc::checked_msg();
                let nsfont = aterm_objc::class(c"NSFont");
                let ours = Id::from_ptr(font.as_ptr().cast_mut());
                let ours_bridged =
                    is_kind(ours, aterm_objc::sel!(isKindOfClass:), nsfont).as_bool();
                let found = system(nsfont.as_id(), aterm_objc::sel!(systemFontOfSize:), 13.0);
                assert!(!found.is_null(), "NSFont has a system font");
                let found_class = aterm_objc::class_name(aterm_objc::class_of(found))
                    .to_string_lossy()
                    .into_owned();
                (ours_bridged, found_class)
            }
        })
    });
    let (ours_bridged, found_class) = answered.unwrap_or_else(|raised| {
        panic!("NSFont raised after this crate made Core Text fonts first: {raised:?}")
    });
    assert!(
        ours_bridged,
        "the Core Text font made before AppKit loaded is not an NSFont"
    );
    println!("{REACHED}{found_class}");
    drop(font);
}
