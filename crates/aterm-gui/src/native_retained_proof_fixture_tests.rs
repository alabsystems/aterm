// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn census_requires_exact_image_multiplicity_and_distinct_backend_processes() {
    // Comparator fixtures only; these values provide no native observation
    // or admission. The actual proof observes processes independently.
    let image = aterm_agent::harness::native_act::Image {
        path: b"/fixture".to_vec(),
        device: 1,
        inode: 2,
    };
    let first = Process {
        pid: 10,
        birth: 1,
        parent: 2,
        group: 3,
        session: 4,
        image: image.clone(),
    };
    let mut second = first.clone();
    second.pid = 11;
    let expected = vec![image.clone(), image.clone()];
    assert!(backend_images_match(
        &[first.clone(), second.clone()],
        &expected,
        20
    ));
    assert!(!backend_images_match(
        std::slice::from_ref(&first),
        &expected,
        20
    ));
    assert!(!backend_images_match(
        &[first.clone(), first.clone()],
        &expected,
        20
    ));
    assert!(!backend_images_match(
        &[first.clone(), second.clone()],
        &[image],
        20
    ));
    assert!(!backend_images_match(
        &[first.clone(), second.clone()],
        &expected,
        10
    ));
    second.image.inode = 3;
    assert!(!backend_images_match(&[first, second], &expected, 20));
}
