// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn reply(code: Option<i32>, stdout: &str, stderr: &str) -> Reply {
    Reply {
        code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

fn png(path: &Path, width: u32, height: u32) {
    let mut bytes = Vec::new();
    {
        let mut encoder = aterm_png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(aterm_png::ColorType::Rgb);
        encoder.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[13, 27, 39].repeat((width * height) as usize))
            .unwrap();
        writer.finish().unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn image_reply(path: &Path, width: usize, height: usize) -> Reply {
    reply(
        Some(0),
        &format!("OK {width} {height} {}\n", path.display()),
        "",
    )
}

#[test]
fn an_ordinary_receipt_requires_its_exact_private_named_decodable_png() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let images = root.join("images");
    std::fs::create_dir(&images).unwrap();
    let path = images.join(BEFORE);
    png(&path, 2, 3);
    assert_eq!(
        image(&image_reply(&path, 2, 3), &root, BEFORE).unwrap(),
        Frame::filled(2, 3, [13, 27, 39])
    );
    assert!(
        image(&image_reply(&path, 3, 2), &root, BEFORE).is_err(),
        "receipt dimensions must agree with decoded pixels"
    );
    assert!(
        image(&image_reply(&path, 2, 3), &root, AFTER).is_err(),
        "another private image is not the named output"
    );
    let outside = root.join("outside.png");
    png(&outside, 2, 3);
    assert!(image(&image_reply(&outside, 2, 3), &root, BEFORE).is_err());
    std::fs::write(&path, b"not a PNG").unwrap();
    assert!(image(&image_reply(&path, 2, 3), &root, BEFORE).is_err());
}

#[test]
fn ordinary_image_status_and_framing_cannot_substitute_for_the_payload() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("images")).unwrap();
    let path = root.join("images").join(BEFORE);
    png(&path, 2, 3);
    let valid = image_reply(&path, 2, 3);
    for code in [None, Some(1), Some(-1)] {
        let mut wrong = valid.clone();
        wrong.code = code;
        assert!(image(&wrong, &root, BEFORE).is_err(), "{wrong:?}");
    }
    for stdout in [
        format!("OK 02 3 {}\n", path.display()),
        format!("OK 2 0 {}\n", path.display()),
        format!("OK 2 3 {}\nOK 2 3 {}\n", path.display(), path.display()),
        format!("OK 2 3 images/{BEFORE}\n"),
        format!("ERR refused\nOK 2 3 {}\n", path.display()),
    ] {
        assert!(
            image(&reply(Some(0), &stdout, ""), &root, BEFORE).is_err(),
            "{stdout}"
        );
    }
}

#[test]
fn ordinary_capture_rejects_a_symlink_leaf_or_image_directory() {
    use std::os::unix::fs::symlink;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let images = root.join("images");
    std::fs::create_dir(&images).unwrap();
    let real = root.join("real.png");
    png(&real, 2, 3);
    let path = images.join(BEFORE);
    symlink(&real, &path).unwrap();
    assert!(image(&image_reply(&path, 2, 3), &root, BEFORE).is_err());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(&images).unwrap();
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    png(&elsewhere.join(BEFORE), 2, 3);
    symlink(&elsewhere, &images).unwrap();
    assert!(image(&image_reply(&images.join(BEFORE), 2, 3), &root, BEFORE).is_err());
}

#[test]
fn a_production_capability_refusal_requires_terminal_status_and_no_side_effects() {
    let message = "ERR presentation capture requires a Linux CPU presentation backend";
    for accepted in [
        reply(Some(1), &format!("{message}\n"), ""),
        reply(Some(2), "", &format!("aterm-ctl: {message}\n")),
    ] {
        assert_eq!(lease_refusal(&accepted, false, false).unwrap(), message);
        assert!(lease_refusal(&accepted, true, false).is_err());
        assert!(lease_refusal(&accepted, false, true).is_err());
        for code in [None, Some(0)] {
            assert!(
                lease_refusal(
                    &Reply {
                        code,
                        ..accepted.clone()
                    },
                    false,
                    false
                )
                .is_err()
            );
        }
    }
    for error in [
        "ERR denied",
        "ERR image requires Owner authority",
        "ERR this CPU backend has no native display submission confirmation",
        "ERR presentation capture requires some other backend",
        "ERR presentation capture requires a Linux CPU presentation backend\nERR denied",
    ] {
        assert!(
            lease_refusal(&reply(Some(1), error, ""), false, false).is_err(),
            "{error}"
        );
    }
    for receipt in [
        "OK presentation-image path=x",
        "OK presentation-released window=0",
    ] {
        for (stdout, stderr) in [
            (format!("{message}\n{receipt}\n"), String::new()),
            (message.into(), format!("aterm-ctl: {receipt}\n")),
        ] {
            assert!(lease_refusal(&reply(Some(1), &stdout, &stderr), false, false).is_err());
        }
    }
    assert!(matches!(
        reply_available(&reply(None, message, "")),
        Err(Fault::Unavailable(_))
    ));
}
