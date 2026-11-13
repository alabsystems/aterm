// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Reply;

#[test]
fn image_paths_keep_spaces_in_the_complete_path() {
    for path in [
        "/tmp/frame.png",
        "/tmp/a world/run/images/frame.png",
        "/tmp/画像 world/frame.png",
    ] {
        let reply = Reply {
            stdout: format!("OK 794 462 {path}\n"),
            ..Reply::default()
        };
        assert_eq!(reply.image_path(), Some(path));
    }
}

#[test]
fn only_successful_sized_plain_images_supply_a_path() {
    for text in [
        "",
        "ERR image failed",
        "OK 0 462 /tmp/frame.png",
        "OK 794 0 /tmp/frame.png",
        "OK width 462 /tmp/frame.png",
        "OK 794 462",
        "OK 794 462 relative.png",
        "OK 794 462 image-meta path=\"/tmp/frame.png\"",
    ] {
        assert_eq!(
            Reply {
                stdout: text.into(),
                ..Reply::default()
            }
            .image_path(),
            None,
            "{text}"
        );
    }
    assert_eq!(
        Reply {
            code: 1,
            stdout: "OK 794 462 /tmp/frame.png\n".into(),
            ..Reply::default()
        }
        .image_path(),
        None
    );
}
