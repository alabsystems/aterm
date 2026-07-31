// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `scheme::load` reads a user theme file from the `XDG_CONFIG_HOME`-resolved
//! theme dir, and reports `NotFound` for an absent name.
//!
//! This is the ONE test that must override `XDG_CONFIG_HOME`, so it runs alone
//! in its own test binary: libtest runs `#[test]`s in a binary in parallel, and
//! `user_theme_dir()` reads the environment on every `load()` call, so an env
//! override inside the 500+-test unit binary races every sibling that calls
//! `load()`. A single `#[test]` per binary makes `std::env::set_var` genuinely
//! safe — no other thread exists to read the environment concurrently (the same
//! convention as the aterm-containment `init_from_env_*` tests and
//! aterm-render's `no_procedural_glyphs_env`).

use aterm_types::Rgb;
use aterm_types::scheme::{MAX_USER_THEME_FILE_BYTES, ThemeError, load};

#[test]
fn load_reads_user_theme_file_then_not_found() {
    // Isolate to a temp config home so the test never touches the real one.
    let tmp = std::env::temp_dir().join(format!(
        "aterm-theme-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let theme_dir = tmp.join("aterm").join("themes");
    std::fs::create_dir_all(&theme_dir).expect("mk theme dir");
    std::fs::write(
        theme_dir.join("Custom.conf"),
        "name = Custom\nforeground = #ddeeff\nbackground = #102030\ncolor1 = #ff0000\n",
    )
    .expect("write theme");
    std::fs::write(
        theme_dir.join("Broken.conf"),
        "foreground = definitely-not-a-colour\n",
    )
    .expect("write invalid theme");
    std::fs::write(theme_dir.join("Notes.txt"), "not a theme").expect("write unrelated file");
    let oversized = std::fs::File::create(theme_dir.join("Huge.conf")).expect("create huge theme");
    oversized
        .set_len((MAX_USER_THEME_FILE_BYTES + 1) as u64)
        .expect("size huge theme");
    drop(oversized);

    // Single `#[test]` in its own integration-test binary — no other thread exists
    // in this process to read the environment concurrently — and routed through the
    // workspace's one lock-scoped env helper regardless. `scoped` restores the
    // previous value (including "was unset") on the way out, on a panic as well as
    // a return, so a failing assertion can never leak the override; note the loads
    // happen INSIDE the scope and the asserts after it, exactly as before.
    let (loaded, missing, huge, traversal) =
        aterm_log::env::scoped("XDG_CONFIG_HOME", &tmp, || {
            (
                load("Custom"),
                load("DoesNotExist_xyz"),
                load("Huge"),
                load("../../outside"),
            )
        });
    let _ = std::fs::remove_dir_all(&tmp);

    let s = loaded.expect("user theme loads");
    assert_eq!(s.name, "Custom");
    assert_eq!(s.foreground, Rgb::new(0xdd, 0xee, 0xff));
    assert_eq!(s.background, Rgb::new(0x10, 0x20, 0x30));
    assert_eq!(s.ansi[1], Rgb::new(0xff, 0x00, 0x00));
    assert!(matches!(missing, Err(ThemeError::NotFound(_))));
    assert!(matches!(huge, Err(ThemeError::Io(message)) if message.contains("limit")));
    assert!(matches!(traversal, Err(ThemeError::Parse { line: 0, .. })));
}
