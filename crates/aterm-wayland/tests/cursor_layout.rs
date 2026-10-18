// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-wayland's `struct wl_cursor` and `struct wl_cursor_image` against
//! `wayland-cursor.h`, on the architecture the test runs on: a probe printing
//! their sizes, alignments and field offsets, compiled with that
//! architecture's C compiler (`CC_<target>`, else `cc`) and run in the Linux
//! lane. Negative control: the probe must say it was built for this
//! architecture. Without a compiler or the header the test says SKIP.

#![cfg(target_os = "linux")]

use std::mem::{align_of, offset_of, size_of};

use aterm_wayland::cursor::{WlCursor, WlCursorImage};

const ARCH: &str = if cfg!(target_arch = "x86_64") {
    "x86_64"
} else {
    "aarch64"
};

fn ours() -> Vec<String> {
    vec![
        format!("arch {ARCH}"),
        format!("size wl_cursor_image {}", size_of::<WlCursorImage>()),
        format!("align wl_cursor_image {}", align_of::<WlCursorImage>()),
        format!(
            "offset wl_cursor_image.width {}",
            offset_of!(WlCursorImage, width)
        ),
        format!(
            "offset wl_cursor_image.height {}",
            offset_of!(WlCursorImage, height)
        ),
        format!(
            "offset wl_cursor_image.hotspot_x {}",
            offset_of!(WlCursorImage, hotspot_x)
        ),
        format!(
            "offset wl_cursor_image.hotspot_y {}",
            offset_of!(WlCursorImage, hotspot_y)
        ),
        format!(
            "offset wl_cursor_image.delay {}",
            offset_of!(WlCursorImage, delay)
        ),
        format!("size wl_cursor {}", size_of::<WlCursor>()),
        format!("align wl_cursor {}", align_of::<WlCursor>()),
        format!(
            "offset wl_cursor.image_count {}",
            offset_of!(WlCursor, image_count)
        ),
        format!("offset wl_cursor.images {}", offset_of!(WlCursor, images)),
        format!("offset wl_cursor.name {}", offset_of!(WlCursor, name)),
    ]
}

const PROBE: &str = r#"#include <stddef.h>
#include <stdio.h>
#include <wayland-cursor.h>
int main(void) {
#if defined(__x86_64__)
  puts("arch x86_64");
#elif defined(__aarch64__)
  puts("arch aarch64");
#else
  puts("arch other");
#endif
  printf("size wl_cursor_image %zu\n", sizeof(struct wl_cursor_image));
  printf("align wl_cursor_image %zu\n", _Alignof(struct wl_cursor_image));
  printf("offset wl_cursor_image.width %zu\n", offsetof(struct wl_cursor_image, width));
  printf("offset wl_cursor_image.height %zu\n", offsetof(struct wl_cursor_image, height));
  printf("offset wl_cursor_image.hotspot_x %zu\n", offsetof(struct wl_cursor_image, hotspot_x));
  printf("offset wl_cursor_image.hotspot_y %zu\n", offsetof(struct wl_cursor_image, hotspot_y));
  printf("offset wl_cursor_image.delay %zu\n", offsetof(struct wl_cursor_image, delay));
  printf("size wl_cursor %zu\n", sizeof(struct wl_cursor));
  printf("align wl_cursor %zu\n", _Alignof(struct wl_cursor));
  printf("offset wl_cursor.image_count %zu\n", offsetof(struct wl_cursor, image_count));
  printf("offset wl_cursor.images %zu\n", offsetof(struct wl_cursor, images));
  printf("offset wl_cursor.name %zu\n", offsetof(struct wl_cursor, name));
  return 0;
}
"#;

#[test]
fn the_cursor_structs_are_the_headers() {
    let target = if cfg!(target_arch = "x86_64") {
        "x86_64_unknown_linux_gnu"
    } else {
        "aarch64_unknown_linux_gnu"
    };
    let cc = std::env::var(format!("CC_{target}")).unwrap_or_else(|_| "cc".to_owned());
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let (src, exe) = (dir.path().join("probe.c"), dir.path().join("probe"));
    std::fs::write(&src, PROBE).unwrap();
    match aterm_uds::spawnfd::command(&cc)
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            eprintln!(
                "SKIP: {cc} could not build the probe (no wayland-cursor.h?):\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        Err(e) => {
            eprintln!("SKIP: no C compiler ({cc}: {e})");
            return;
        }
    }
    let ran = aterm_uds::spawnfd::command(&exe)
        .output()
        .expect("the probe runs");
    let theirs: Vec<String> = String::from_utf8(ran.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        theirs.first(),
        ours().first(),
        "built for this architecture"
    );
    assert_eq!(theirs, ours());
}
